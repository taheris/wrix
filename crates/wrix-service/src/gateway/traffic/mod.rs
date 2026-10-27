//! Rama HTTP/1 streaming admission with connection-owned cancellation.

use super::{Error, Secret, State, cancelled, policy::Host};
use base64::Engine;
use rama_core::service::service_fn;
use rama_http::{Body, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use rama_http_core::{
    body::Incoming, client::conn::http1 as client, server::conn::http1 as server,
};
use std::{
    convert::Infallible,
    net::SocketAddr,
    num::NonZeroU16,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinSet,
    time::timeout,
};

const BUDGET: Duration = Duration::from_secs(15);
type Children = Arc<Mutex<JoinSet<()>>>;

pub(super) async fn serve(
    socket: TcpStream,
    state: State,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<(), Error> {
    let (identity, mut selected) = watch::channel(None::<Secret>);
    let children = Arc::new(Mutex::new(JoinSet::new()));
    let requests = Arc::new(Semaphore::new(8));
    let service = service_fn({
        let state = state.clone();
        let children = children.clone();
        move |request: Request<Incoming>| {
            let state = state.clone();
            let identity = identity.clone();
            let children = children.clone();
            let requests = requests.clone();
            let permit = permit.clone();
            async move {
                let response =
                    match admit(request, &state, &identity, &children, requests, permit).await {
                        Ok(response) => response,
                        Err(error) => failure(&error),
                    };
                Ok::<_, Infallible>(response)
            }
        }
    });
    let builder = server::Builder::new()
        .with_max_headers(64)
        .with_header_read_timeout(BUDGET)
        .try_with_max_buf_size(32768)
        .map_err(std::io::Error::other)?;
    let connection = builder
        .serve_connection(rama_core::ServiceInput::new(socket), service)
        .with_upgrades();
    let revoke = async {
        let secret = loop {
            let selection = selected.borrow_and_update().clone();
            if let Some(secret) = selection {
                break secret;
            }
            if selected.changed().await.is_err() {
                return;
            }
        };
        if let Ok(grant) = state.grant(&secret) {
            cancelled(grant.cancel.subscribe()).await;
        }
    };
    tokio::pin!(revoke);
    tokio::select! {
        () = &mut revoke => return Ok(()),
        result = connection => { result.map_err(std::io::Error::other)?; }
    }
    let mut children = std::mem::take(&mut *children.lock().map_err(|_| Error::Poisoned)?);
    loop {
        tokio::select! {
            () = &mut revoke => return Ok(()),
            result = children.join_next() => match result {
                None => return Ok(()),
                Some(Ok(())) => (),
                Some(Err(error)) => return Err(std::io::Error::other(error).into()),
            }
        }
    }
}

fn credential(headers: &HeaderMap) -> Result<Secret, Error> {
    if headers.get_all("proxy-authorization").iter().count() != 1 {
        return Err(Error::Denied);
    }
    let value = headers
        .get("proxy-authorization")
        .ok_or(Error::Denied)?
        .to_str()
        .map_err(|_| Error::Denied)?;
    if let Some(value) = value.strip_prefix("Bearer ") {
        return Secret::try_from(value.to_owned()).map_err(|_| Error::Denied);
    }
    let encoded = value.strip_prefix("Basic ").ok_or(Error::Denied)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| Error::Denied)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Denied)?;
    Secret::try_from(text.strip_prefix("wrix:").ok_or(Error::Denied)?.to_owned())
        .map_err(|_| Error::Denied)
}

fn destination(request: &Request<Incoming>) -> Result<(Host, NonZeroU16), Error> {
    let uri = request.uri();
    if uri.userinfo().is_some() || uri.fragment().is_some() {
        return Err(Error::Invalid);
    }
    let connect = request.method() == Method::CONNECT;
    if connect && (uri.path().is_some_and(|path| !path.is_empty()) || uri.query().is_some()) {
        return Err(Error::Invalid);
    }
    if (!connect && uri.scheme_str() != Some("http")) || (connect && uri.scheme().is_some()) {
        return Err(Error::Invalid);
    }
    let host = Host::parse(&uri.host_str().ok_or(Error::Invalid)?)?;
    let port = NonZeroU16::new(uri.port_u16().unwrap_or(if connect { 0 } else { 80 }))
        .ok_or(Error::Invalid)?;
    if request.headers().get_all("host").iter().count() != 1 {
        return Err(Error::Invalid);
    }
    let header = request
        .headers()
        .get("host")
        .ok_or(Error::Invalid)?
        .to_str()
        .map_err(|_| Error::Invalid)?;
    let (header_host, header_port) = match header.split_once(':') {
        Some((host, port)) => (
            Host::parse(host)?,
            port.parse::<NonZeroU16>().map_err(|_| Error::Invalid)?,
        ),
        None if !connect => (
            Host::parse(header)?,
            NonZeroU16::new(80).ok_or(Error::Invalid)?,
        ),
        None => return Err(Error::Invalid),
    };
    if host != header_host || port != header_port {
        return Err(Error::Invalid);
    }
    let headers = request.headers();
    if headers.contains_key("upgrade")
        || headers.contains_key("expect")
        || (headers.contains_key("transfer-encoding") && headers.contains_key("content-length"))
        || headers.get_all("content-length").iter().count() > 1
    {
        return Err(Error::Invalid);
    }
    if connect
        && (headers.contains_key("transfer-encoding")
            || headers
                .get("content-length")
                .is_some_and(|v| v.as_bytes() != b"0"))
    {
        return Err(Error::Invalid);
    }
    Ok((host, port))
}

fn strip_hop(headers: &mut HeaderMap) -> Result<(), Error> {
    let mut names = Vec::new();
    for value in &headers.get_all("connection") {
        for name in value.to_str().map_err(|_| Error::Invalid)?.split(',') {
            let name = name.trim().to_ascii_lowercase();
            if [
                "host",
                "content-length",
                "transfer-encoding",
                "proxy-authorization",
            ]
            .contains(&name.as_str())
            {
                return Err(Error::Invalid);
            }
            names.push(name);
        }
    }
    for name in names {
        headers.remove(name.as_str());
    }
    for name in [
        "connection",
        "proxy-connection",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "te",
        "trailer",
        "upgrade",
    ] {
        headers.remove(name);
    }
    Ok(())
}

async fn dial(state: &State, host: &Host, port: NonZeroU16) -> Result<TcpStream, Error> {
    let addresses = state.network.resolve(host, port.get()).await?;
    if addresses.is_empty() || addresses.len() > 64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "gateway resolution has no bounded destination set",
        )
        .into());
    }
    if addresses
        .iter()
        .any(|address| !state.baseline.permits(*address) || state.protected.contains(address))
    {
        return Err(Error::Protected);
    }
    let address: SocketAddr = addresses[0];
    state.network.connect(address).await
}

async fn admit(
    mut request: Request<Incoming>,
    state: &State,
    identity: &watch::Sender<Option<Secret>>,
    children: &Children,
    requests: Arc<Semaphore>,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<Response, Error> {
    let secret = credential(request.headers())?;
    let grant = state.grant(&secret)?;
    if identity.borrow().as_ref().is_some_and(|old| old != &secret) {
        return Err(Error::Denied);
    }
    identity.send_if_modified(|bound| {
        if bound.is_none() {
            *bound = Some(secret);
            true
        } else {
            false
        }
    });
    let (host, port) = destination(&request)?;
    if !grant.policy.permits(&host, port) {
        return Ok(response(StatusCode::FORBIDDEN));
    }
    if grant.probe {
        return if request.method() == Method::GET && request.uri().path_or_root() == "/" {
            Ok(response(StatusCode::NO_CONTENT))
        } else {
            Err(Error::Invalid)
        };
    }
    if request.method() != Method::CONNECT {
        strip_hop(request.headers_mut())?;
    }
    let admission_cancel = grant.cancel.subscribe();
    let work = async {
        let request_permit = requests.try_acquire_owned().map_err(|_| Error::Capacity)?;
        let mut upstream = timeout(BUDGET, dial(state, &host, port))
            .await
            .map_err(std::io::Error::other)??;
        if *grant.cancel.borrow() {
            return Err(Error::Denied);
        }
        if request.method() == Method::CONNECT {
            let upgrade = rama_http::io::upgrade::handle_upgrade(&mut request);
            spawn(children, async move {
                let _permit = permit;
                let _request_permit = request_permit;
                let relay = async {
                    let mut downstream = upgrade.await.map_err(std::io::Error::other)?;
                    tokio::io::copy_bidirectional_with_sizes(
                        &mut downstream,
                        &mut upstream,
                        16384,
                        16384,
                    )
                    .await?;
                    Ok::<_, std::io::Error>(())
                };
                tokio::select! {
                    () = cancelled(grant.cancel.subscribe()) => (),
                    result = relay => { if result.is_err() { tracing::debug!(role = "egress-connect", "gateway CONNECT relay failed"); } }
                }
            })?;
            return Ok(response(StatusCode::OK));
        }
        request
            .headers_mut()
            .insert("connection", HeaderValue::from_static("close"));
        let path = format!(
            "{}{}",
            request.uri().path_or_root(),
            request
                .uri()
                .query()
                .map_or_else(String::new, |q| format!("?{q}"))
        );
        *request.uri_mut() =
            rama_net::uri::Uri::parse_reference(path).map_err(|_| Error::Invalid)?;
        let builder = client::Builder::new()
            .with_max_headers(64)
            .try_with_max_buf_size(32768)
            .map_err(std::io::Error::other)?;
        let (mut sender, connection) = builder
            .handshake(rama_core::ServiceInput::new(upstream))
            .await
            .map_err(std::io::Error::other)?;
        let cancel = grant.cancel.subscribe();
        spawn(children, async move {
            let _permit = permit;
            let _request_permit = request_permit;
            tokio::select! {
                () = cancelled(cancel) => (),
                result = connection => { if result.is_err() { tracing::debug!(role = "egress-http", "gateway upstream failed"); } }
            }
        })?;
        let mut response = timeout(BUDGET, sender.send_request(request))
            .await
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            return Err(Error::Invalid);
        }
        strip_hop(response.headers_mut())?;
        Ok(response.map(Body::new))
    };
    tokio::select! { biased; () = cancelled(admission_cancel) => Err(Error::Denied), result = work => result }
}

fn spawn(
    children: &Children,
    future: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Error> {
    let mut children = children.lock().map_err(|_| Error::Poisoned)?;
    while let Some(result) = children.try_join_next() {
        result.map_err(std::io::Error::other)?;
    }
    children.spawn(future);
    Ok(())
}
fn response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}
fn failure(error: &Error) -> Response {
    let mut response = response(match error {
        Error::Denied => StatusCode::PROXY_AUTHENTICATION_REQUIRED,
        Error::Invalid | Error::Json(_) => StatusCode::BAD_REQUEST,
        Error::Protected => StatusCode::FORBIDDEN,
        Error::Capacity => StatusCode::SERVICE_UNAVAILABLE,
        Error::Io(_) | Error::Poisoned => StatusCode::BAD_GATEWAY,
    });
    response
        .headers_mut()
        .insert("connection", HeaderValue::from_static("close"));
    if response.status() == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        response.headers_mut().insert(
            "proxy-authenticate",
            HeaderValue::from_static("Basic realm=\"wrix\""),
        );
    }
    response
}
