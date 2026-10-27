//! System resolution and checked numeric dialing; external I/O is replaceable in conformance tests.

use super::{Error, policy::Host};
use std::{future::Future, net::SocketAddr, pin::Pin};
use tokio::net::TcpStream;

type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;
pub(super) trait Network: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a Host, port: u16) -> Pending<'a, Vec<SocketAddr>>;
    fn connect(&self, address: SocketAddr) -> Pending<'_, TcpStream>;
}
pub(super) struct System;
impl Network for System {
    fn resolve<'a>(&'a self, host: &'a Host, port: u16) -> Pending<'a, Vec<SocketAddr>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .take(65)
                .collect())
        })
    }
    fn connect(&self, address: SocketAddr) -> Pending<'_, TcpStream> {
        Box::pin(async move { Ok(TcpStream::connect(address).await?) })
    }
}
