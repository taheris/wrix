//! Native response work remains counted through the actual HTTP transport flush.
use super::state::Guard;
use axum::{extract::connect_info::Connected, serve::IncomingStream};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::watch,
};

#[derive(Clone)]
pub(super) struct State(Arc<Mutex<Pending>>);
struct Pending {
    closed: bool,
    response: Vec<Guard>,
}
impl State {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Pending {
            closed: false,
            response: Vec::new(),
        })))
    }
    fn lock(&self) -> MutexGuard<'_, Pending> {
        match self.0.lock() {
            Ok(pending) => pending,
            Err(poisoned) => {
                super::fail();
                poisoned.into_inner()
            }
        }
    }
    fn defer(&self, work: Guard) {
        let mut pending = self.lock();
        if !pending.closed {
            pending.response.push(work);
        }
    }
    fn flushed(&self) {
        self.lock().response.clear();
    }
    fn close(&self) {
        let mut pending = self.lock();
        pending.closed = true;
        pending.response.clear();
    }
}
impl Connected<IncomingStream<'_, Listener>> for State {
    fn connect_info(stream: IncomingStream<'_, Listener>) -> Self {
        stream.io().state.clone()
    }
}

pub(super) struct ResponseWork {
    work: Option<Guard>,
    transport: Option<State>,
}
impl ResponseWork {
    pub fn new(work: Guard, transport: Option<State>) -> Self {
        Self {
            work: Some(work),
            transport,
        }
    }
}
impl Drop for ResponseWork {
    fn drop(&mut self) {
        if let (Some(work), Some(transport)) = (self.work.take(), self.transport.as_ref()) {
            transport.defer(work);
        }
    }
}

pub(super) struct Listener {
    listener: TcpListener,
    signal: watch::Receiver<bool>,
}
impl Listener {
    pub fn new(listener: TcpListener, signal: watch::Receiver<bool>) -> Self {
        Self { listener, signal }
    }
}
impl axum::serve::Listener for Listener {
    type Io = Stream;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, address) = axum::serve::Listener::accept(&mut self.listener).await;
        let mut signal = self.signal.clone();
        let closing = Box::pin(async move {
            while !*signal.borrow_and_update() {
                if signal.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        });
        (
            Stream {
                stream,
                state: State::new(),
                closing,
                closed: false,
            },
            address,
        )
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

pub(super) struct Stream {
    stream: TcpStream,
    state: State,
    closing: Pin<Box<dyn Future<Output = ()> + Send>>,
    closed: bool,
}
impl Stream {
    fn should_close(&mut self, context: &mut Context<'_>) -> bool {
        if self.closed || self.closing.as_mut().poll(context).is_ready() {
            self.closed = true;
            self.state.close();
            true
        } else {
            false
        }
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.should_close(context) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.should_close(context) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "native work drained",
            )));
        }
        Pin::new(&mut self.stream).poll_write(context, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_flush(context);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.state.flushed();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.state.close();
    }
}
