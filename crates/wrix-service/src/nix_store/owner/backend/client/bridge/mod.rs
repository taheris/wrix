//! Bounded untrusted transport to one delivery's loopback SSH port, never management or arbitrary TCP.
pub mod broker;
mod protocol;
#[cfg(test)]
mod test;
use super::Error;
use protocol::{Frame, Kind};
use std::{collections::BTreeMap, num::NonZeroU16, process::Stdio};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};

/// Observer only. Drop cannot signal, wait for or stop the independently supervised broker.
pub struct Bridge {
    _epoch: crate::nix_store::native::lease::Id,
}
impl wrix_sandbox::nix_store::Transport for Bridge {}

struct Task(tokio::task::AbortHandle);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Stream {
    sender: mpsc::Sender<Frame>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    command: std::process::Command,
    port: NonZeroU16,
    mut stop: oneshot::Receiver<()>,
    ready: oneshot::Sender<()>,
) -> Result<(), Error> {
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdout.take().ok_or(Error::Invalid)?;
    let mut output = child.stdin.take().ok_or(Error::Invalid)?;
    let mut greeting = [0; 8];
    tokio::select! {
        result = tokio::time::timeout(std::time::Duration::from_secs(30), input.read_exact(&mut greeting)) => { result.map_err(|_| Error::Retained)??; }
        _ = &mut stop => return Ok(()),
    }
    if &greeting != b"WRIXNB01" {
        return Err(Error::Invalid);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        output.write_all(b"WRIXNB01"),
    )
    .await
    .map_err(|_| Error::Retained)??;
    ready.send(()).map_err(|()| Error::Retained)?;
    let (incoming, mut received) = mpsc::channel(2);
    let _reader = Task(
        tokio::spawn(async move {
            loop {
                let result = Frame::read(&mut input).await;
                let failed = result.is_err();
                if incoming.send(result).await.is_err() || failed {
                    break;
                }
            }
        })
        .abort_handle(),
    );
    let (outbound, mut frames) = mpsc::channel::<Frame>(32);
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            frame.write(&mut output).await?;
        }
        Ok::<(), Error>(())
    });
    let _writer = Task(writer.abort_handle());
    let (finished, mut completion) = mpsc::channel(16);
    let mut streams: BTreeMap<u32, Stream> = BTreeMap::new();
    let mut last = 0;
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => return Ok(()),
            result = child.wait() => { result?; return Err(Error::Retained); }
            result = &mut writer => { result.map_err(std::io::Error::other)??; return Err(Error::Retained); }
            Some(id) = completion.recv() => { streams.remove(&id); }
            frame = received.recv() => {
                let frame = frame.ok_or(Error::Retained)??;
                if frame.kind == Kind::Open {
                    if frame.id <= last || streams.len() >= 16 { return Err(Error::Invalid); }
                    last = frame.id;
                    let (sender, requests) = mpsc::channel(4);
                    let outbound = outbound.clone();
                    let finished = finished.clone();
                    let id = frame.id;
                    let task = tokio::spawn(async move {
                        if let Err(error) = stream(port, id, requests, &outbound).await { tracing::debug!(operation = "apple_nix_bridge_stream", id, %error, "fixed SSH transport stream ended"); }
                        if outbound.send(Frame::empty(Kind::Close, id)).await.is_err() { return; }
                        if finished.send(id).await.is_err() { tracing::debug!(operation = "apple_nix_bridge_stream", id, "broker already stopped"); }
                    });
                    streams.insert(id, Stream { sender, task });
                } else if frame.id > last { return Err(Error::Invalid); }
                else if let Some(stream) = streams.get(&frame.id) {
                    stream.sender.try_send(frame).map_err(|_| Error::Invalid)?;
                }
            }
        }
    }
}

async fn stream(
    port: NonZeroU16,
    id: u32,
    inbound: mpsc::Receiver<Frame>,
    outbound: &mpsc::Sender<Frame>,
) -> Result<(), Error> {
    let socket = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port.get())),
    )
    .await
    .map_err(|_| Error::Retained)??;
    socket.set_nodelay(true)?;
    forward(socket, id, inbound, outbound).await
}

async fn forward(
    socket: TcpStream,
    id: u32,
    mut inbound: mpsc::Receiver<Frame>,
    outbound: &mpsc::Sender<Frame>,
) -> Result<(), Error> {
    let (mut reader, writer) = socket.into_split();
    let mut credit = true;
    let mut pending: Option<(Vec<u8>, usize)> = None;
    let mut bytes = vec![0; protocol::CHUNK];
    loop {
        tokio::select! {
            frame = inbound.recv() => {
                let frame = frame.ok_or(Error::Retained)?;
                match frame.kind {
                    Kind::Data if pending.is_none() => pending = Some((frame.data, 0)),
                    Kind::Credit if !credit => credit = true,
                    Kind::Close => return Ok(()),
                    _ => return Err(Error::Invalid),
                }
            }
            result = writer.writable(), if pending.is_some() => {
                result?;
                let (data, offset) = pending.as_mut().ok_or(Error::Invalid)?;
                match writer.try_write(&data[*offset..]) {
                    Ok(0) => return Err(Error::Retained),
                    Ok(count) => *offset += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Err(error.into()),
                }
                if *offset == data.len() {
                    pending = None;
                    outbound.send(Frame::empty(Kind::Credit, id)).await.map_err(|_| Error::Retained)?;
                }
            }
            count = reader.read(&mut bytes), if credit => {
                let count = count?;
                if count == 0 { return Ok(()); }
                outbound.send(Frame { kind: Kind::Data, id, data: bytes[..count].to_vec() }).await.map_err(|_| Error::Retained)?;
                credit = false;
            }
        }
    }
}
