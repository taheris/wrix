//! Private native management with authenticated publication admission.
use crate::{RequestState, State, StateInner, config::Config};
use anyhow::{Context, Result, bail};
use attic::cache::CacheName;
use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, Path as RoutePath, Request},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::post,
};
use futures::{FutureExt, StreamExt};
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixListener},
    sync::watch,
};

#[path = "work_drain_state.rs"]
pub mod state;
#[path = "work_drain_transport.rs"]
mod transport;
use state::{Guard, Tracker};

static TRACKER: OnceLock<Arc<Tracker>> = OnceLock::new();
static MANAGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn tracker() -> &'static Arc<Tracker> {
    TRACKER.get_or_init(Tracker::new)
}
pub fn background() -> Guard {
    tracker().background()
}
pub fn fail() {
    tracker().fail();
}

/// Fixed private-role command; it does not load configuration or open native data.
pub async fn invoke(command: Command) -> Result<Vec<u8>> {
    let socket = std::env::var_os("WRIX_CELLER_DRAIN_SOCKET")
        .unwrap_or_else(|| "/run/wrix/celler/drain.sock".into());
    invoke_at(Path::new(&socket), command).await
}

async fn invoke_at(socket: &Path, command: Command) -> Result<Vec<u8>> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    let mut request = serde_json::to_vec(&serde_json::json!({"command": command}))?;
    request.push(b'\n');
    stream.write_all(&request).await?;
    let mut response = Vec::new();
    BufReader::new(stream)
        .take(4097)
        .read_until(b'\n', &mut response)
        .await?;
    if response.len() > 4096 {
        bail!("oversized native work-drain response");
    }
    let value: serde_json::Value = serde_json::from_slice(&response)?;
    if value["accepted"] != true {
        bail!("native work-drain command was not accepted");
    }
    Ok(response)
}

pub fn routes() -> Router {
    Router::new().route("/_api/v1/workflow/{cache}", post(open).delete(close))
}

fn authority(headers: &HeaderMap) -> &str {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

async fn authorize(
    _state: &State,
    req: &RequestState,
    cache: &CacheName,
) -> crate::error::ServerResult<()> {
    req.auth
        .get_permission_for_cache(cache, false)
        .require_push()?;
    Ok(())
}

async fn open(
    Extension(state): Extension<State>,
    Extension(req): Extension<RequestState>,
    RoutePath(cache): RoutePath<CacheName>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authorize(&state, &req, &cache).await {
        return error.into_response();
    }
    let id = uuid::Uuid::new_v4().to_string();
    if !tracker().open_workflow(id.clone(), authority(&headers).to_owned()) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(serde_json::json!({"workflow": id})).into_response()
}

async fn close(
    Extension(state): Extension<State>,
    Extension(req): Extension<RequestState>,
    RoutePath(cache): RoutePath<CacheName>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authorize(&state, &req, &cache).await {
        return error.into_response();
    }
    let id = headers
        .get("x-wrix-workflow")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !tracker().close_workflow(id, authority(&headers)) {
        return StatusCode::CONFLICT.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn admission(req: Request, next: Next) -> Response {
    let workflow = req
        .headers()
        .get("x-wrix-workflow")
        .and_then(|value| value.to_str().ok());
    let mutation = !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    let opening = req.method() == axum::http::Method::POST
        && req.uri().path().starts_with("/_api/v1/workflow/");
    if MANAGED.load(std::sync::atomic::Ordering::Acquire)
        && mutation
        && workflow.is_none()
        && !opening
    {
        return StatusCode::PRECONDITION_REQUIRED.into_response();
    }
    let transport = req
        .extensions()
        .get::<axum::extract::ConnectInfo<transport::State>>()
        .map(|info| info.0.clone());
    if MANAGED.load(std::sync::atomic::Ordering::Acquire) && transport.is_none() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(guard) = tracker().request(workflow, authority(req.headers())) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let native = tokio::spawn(async move {
        let response = match std::panic::AssertUnwindSafe(next.run(req))
            .catch_unwind()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                fail();
                tracing::error!(
                    role = "celler",
                    "Native request panicked; completion is uncertain"
                );
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
        let (parts, body) = response.into_parts();
        let work = transport::ResponseWork::new(guard, transport);
        let stream = async_stream::stream! {
            let _work = work;
            let mut body = body.into_data_stream();
            while let Some(chunk) = body.next().await { yield chunk; }
        };
        Response::from_parts(parts, Body::from_stream(stream))
    });
    match native.await {
        Ok(response) => response,
        Err(error) => {
            fail();
            tracing::error!(%error, "Native request completion is uncertain");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestCommand {
    command: Command,
}
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Begin,
    Quiesce,
    Status,
    Abort,
    Release,
}

/// The socket is container-local and must never be forwarded to workers.
pub async fn run(
    listen: Option<std::net::SocketAddr>,
    config: Config,
    socket: &Path,
    gc: bool,
) -> Result<()> {
    MANAGED.store(true, std::sync::atomic::Ordering::Release);
    let state = StateInner::new(config).await;
    state.database().await?;
    let listener = TcpListener::bind(listen.unwrap_or(state.config.listen)).await?;
    let management = UnixListener::bind(socket).context("bind private native work-drain socket")?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let (stop, shutdown) = watch::channel(false);
    let (finished, completion) = tokio::sync::oneshot::channel::<bool>();
    let manager = tokio::spawn(control(management, stop, completion));
    let background = tokio::spawn(maintenance(state.clone(), shutdown.clone(), gc));
    let mut signal = shutdown;
    let listener = transport::Listener::new(listener, signal.clone());
    let serving = axum::serve(
        listener,
        crate::router(state.clone()).into_make_service_with_connect_info::<transport::State>(),
    )
    .with_graceful_shutdown(async move {
        while !*signal.borrow_and_update() {
            if signal.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    })
    .await;
    let result = async {
        serving?;
        background.await??;
        if tracker().status().failed {
            bail!("native background metadata completion failed");
        }
        close_database(&state).await?;
        tracker().released();
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if finished.send(result.is_ok()).is_err() {
        bail!("native release observer lost; closure outcome uncertain");
    }
    manager.await??;
    std::fs::remove_file(socket)?;
    result
}

async fn close_database(state: &State) -> Result<()> {
    if let Some(db) = state.database.get() {
        if db.get_database_backend() == sea_orm::DatabaseBackend::Sqlite {
            db.execute_unprepared("PRAGMA synchronous=FULL;").await?;
            let checkpoint = db
                .query_one_raw(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Sqlite,
                    "PRAGMA wal_checkpoint(TRUNCATE);".to_owned(),
                ))
                .await?;
            let busy: i32 = checkpoint
                .context("missing checkpoint result")?
                .try_get_by_index(0)?;
            if busy != 0 {
                bail!("SQLite checkpoint remained busy");
            }
        }
        db.clone().close().await?;
    }
    Ok(())
}

async fn control(
    listener: UnixListener,
    stop: watch::Sender<bool>,
    completion: tokio::sync::oneshot::Receiver<bool>,
) -> Result<()> {
    let mut completion = Some(completion);
    loop {
        let (stream, _) = listener.accept().await?;
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(5),
            (&mut stream).take(4097).read_line(&mut line),
        )
        .await;
        let size = match read {
            Ok(Ok(size)) => size,
            Ok(Err(error)) => {
                tracing::warn!(%error, "Native lifecycle request read failed");
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, "Native lifecycle request read timed out");
                continue;
            }
        };
        let command = if size > 4096 {
            None
        } else {
            serde_json::from_str::<RequestCommand>(&line).ok()
        };
        let mut accepted = false;
        let mut release = false;
        if let Some(request) = command {
            accepted = match request.command {
                Command::Begin => tracker().begin(),
                Command::Quiesce => tracker().quiescent(),
                Command::Status => true,
                Command::Abort => tracker().abort(),
                Command::Release => {
                    release = tracker().release();
                    release
                }
            };
        }
        if release {
            stop.send(true)?;
            accepted = completion.take().context("duplicate release")?.await?;
        }
        let mut response = serde_json::to_vec(
            &serde_json::json!({"accepted": accepted, "status": tracker().status()}),
        )?;
        response.push(b'\n');
        let sent = async {
            stream.get_mut().write_all(&response).await?;
            stream.get_mut().shutdown().await
        }
        .await;
        if release {
            sent?;
            return Ok(());
        }
        if let Err(error) = sent {
            tracing::warn!(%error, "Native lifecycle observer disconnected");
        }
    }
}

async fn maintenance(state: State, mut shutdown: watch::Receiver<bool>, gc: bool) -> Result<()> {
    let interval = state.config.garbage_collection.interval;
    let mut next_collection = tokio::time::Instant::now();
    loop {
        if *shutdown.borrow_and_update() {
            return Ok(());
        }
        if let Some(_work) = tracker().maintenance() {
            if gc && !interval.is_zero() && tokio::time::Instant::now() >= next_collection {
                next_collection = tokio::time::Instant::now() + interval;
                if let Err(error) = crate::gc::collect_state(&state).await {
                    fail();
                    tracing::error!(%error, "Native GC failed; release blocked");
                }
            }
            if state.config.database.heartbeat {
                state
                    .database()
                    .await
                    .map_err(|error| {
                        fail();
                        error
                    })?
                    .execute_unprepared("SELECT 'heartbeat';")
                    .await
                    .map_err(|error| {
                        fail();
                        error
                    })?;
            }
        }
        let delay = if gc && !interval.is_zero() {
            interval.min(Duration::from_secs(60))
        } else {
            Duration::from_secs(60)
        };
        tokio::select! {
            _ = tokio::time::sleep(delay) => (),
            changed = shutdown.changed() => { changed.map_err(|error| { fail(); error })?; },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    async fn command(path: &Path, command: &str) -> serde_json::Value {
        let command = serde_json::from_value(serde_json::json!(command)).unwrap();
        let response = invoke_at(path, command).await.unwrap();
        serde_json::from_slice(&response).unwrap()
    }

    async fn http(
        address: std::net::SocketAddr,
        method: &str,
        route: &str,
        token: &str,
        workflow: Option<&str>,
    ) -> String {
        http_body(address, method, route, token, workflow, "", &[]).await
    }

    async fn http_body(
        address: std::net::SocketAddr,
        method: &str,
        route: &str,
        token: &str,
        workflow: Option<&str>,
        headers: &str,
        body: &[u8],
    ) -> String {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let workflow = workflow
            .map(|id| format!("X-Wrix-Workflow: {id}\r\n"))
            .unwrap_or_default();
        stream.write_all(format!("{method} {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n{workflow}{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }

    #[tokio::test]
    async fn native_release_ack_waits_for_clean_storage_completion() {
        MANAGED.store(true, std::sync::atomic::Ordering::Release);
        let directory =
            std::env::temp_dir().join(format!("celler-drain-storage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "database": {"url": format!("sqlite://{}?mode=rwc", directory.join("metadata.db").display()), "heartbeat": false},
            "storage": {"type": "local", "path": directory.join("archive")},
            "chunking": {"nar-size-threshold": 1, "min-size": 64, "avg-size": 256, "max-size": 1024},
            "jwt": {}
        })).unwrap();
        let cleanup_work = background();
        let canceled = attic::util::Finally::new(async move {
            drop(cleanup_work);
        });
        assert_eq!(tracker().status().background, 1);
        canceled.cancel();
        assert_eq!(tracker().status().background, 0);
        let secret = b"native-drain-fixture-not-a-live-key";
        config.jwt.hs256_secret_key_file = Some(attic_token::HS256Key::from_bytes(secret));
        let signing = attic_token::SignatureType::HS256(attic_token::HS256Key::from_bytes(secret));
        let namespace = CacheName::new("private".into()).unwrap();
        let make_token = |push| {
            let mut token = attic_token::Token::new(
                "fixture".into(),
                &(chrono::Utc::now() + chrono::Duration::minutes(5)),
            );
            let permission = token.get_or_insert_permission_mut(namespace.to_pattern());
            permission.pull = true;
            permission.push = push;
            permission.create_cache = push;
            token.encode(&signing, &None, &None).unwrap()
        };
        let push = make_token(true);
        let pull = make_token(false);
        crate::run_migrations(config.clone()).await.unwrap();
        let state = StateInner::new(config.clone()).await;
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = http_listener.local_addr().unwrap();
        let (stop, mut stopped) = watch::channel(false);
        let router =
            crate::router(state.clone()).into_make_service_with_connect_info::<transport::State>();
        let http_listener = transport::Listener::new(http_listener, stopped.clone());
        let mut http_signal = stopped.clone();
        let http_server = tokio::spawn(async move {
            axum::serve(http_listener, router)
                .with_graceful_shutdown(async move {
                    while !*http_signal.borrow_and_update() {
                        http_signal.changed().await.unwrap();
                    }
                })
                .await
                .unwrap();
        });
        let denied = http(address, "POST", "/_api/v1/workflow/private", &pull, None).await;
        assert!(!denied.starts_with("HTTP/1.1 200"));
        let untracked = http(address, "PUT", "/_api/v1/upload-path", &push, None).await;
        assert!(untracked.starts_with("HTTP/1.1 428"));
        let admitted = http(address, "POST", "/_api/v1/workflow/private", &push, None).await;
        assert!(admitted.starts_with("HTTP/1.1 200"), "{admitted}");
        let body: serde_json::Value = serde_json::from_str(
            &admitted[admitted.find('{').unwrap()..=admitted.rfind('}').unwrap()],
        )
        .unwrap();
        let workflow = body["workflow"].as_str().unwrap();
        let cache = attic::api::v1::cache_config::CreateCacheRequest {
            keypair: attic::api::v1::cache_config::KeypairConfig::Generate,
            is_public: false,
            store_dir: "/nix/store".into(),
            priority: 40,
            upstream_cache_key_names: vec![],
        };
        let created = http_body(
            address,
            "POST",
            "/_api/v1/cache-config/private",
            &push,
            Some(workflow),
            "Content-Type: application/json\r\n",
            &serde_json::to_vec(&cache).unwrap(),
        )
        .await;
        assert!(created.starts_with("HTTP/1.1 200"), "{created}");
        let nar = include_bytes!(
            "../../attic/src/nix_store/tests/nar/nm1w9sdm6j6icmhd2q3260hl1w9zj6li-attic-test-no-deps.nar"
        );
        use sha2::Digest;
        let hash = attic::hash::Hash::Sha256(sha2::Sha256::digest(nar).into());
        let metadata = serde_json::json!({
            "cache": "private", "store_path_hash": "nm1w9sdm6j6icmhd2q3260hl1w9zj6li",
            "store_path": "/nix/store/nm1w9sdm6j6icmhd2q3260hl1w9zj6li-attic-test-no-deps",
            "references": [], "system": null, "deriver": null, "sigs": [], "ca": null,
            "nar_hash": hash, "nar_size": nar.len()
        });
        let mut payload = serde_json::to_vec(&metadata).unwrap();
        let headers = format!(
            "{}: {}\r\n",
            attic::api::v1::upload_path::CELLER_NAR_INFO_PREAMBLE_SIZE,
            payload.len()
        );
        payload.extend_from_slice(nar);
        let uploaded = http_body(
            address,
            "PUT",
            "/_api/v1/upload-path",
            &push,
            Some(workflow),
            &headers,
            &payload,
        )
        .await;
        assert!(uploaded.starts_with("HTTP/1.1 200"), "{uploaded}");
        let info = http(
            address,
            "GET",
            "/private/nm1w9sdm6j6icmhd2q3260hl1w9zj6li.narinfo",
            &pull,
            None,
        )
        .await;
        assert!(info.starts_with("HTTP/1.1 200"), "{info}");
        let completed = tokio::time::Instant::now();
        while tracker().status().background != 0 {
            assert!(completed.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let path =
            std::env::temp_dir().join(format!("celler-native-drain-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&path).unwrap();
        let (finished, completion) = tokio::sync::oneshot::channel();
        let manager = tokio::spawn(control(listener, stop, completion));
        let status = command(&path, "begin").await;
        assert_eq!(status["status"]["phase"], "draining");
        assert_eq!(status["status"]["requests"], 0);
        assert_eq!(status["status"]["workflows"], 1);
        assert!(invoke_at(&path, Command::Release).await.is_err());
        let rejected = http(address, "POST", "/_api/v1/workflow/private", &push, None).await;
        assert!(rejected.starts_with("HTTP/1.1 503"));
        let continuation = http(address, "GET", "/still-admitted", &push, Some(workflow)).await;
        assert!(continuation.starts_with("HTTP/1.1 404"));
        let closed = http(
            address,
            "DELETE",
            "/_api/v1/workflow/private",
            &push,
            Some(workflow),
        )
        .await;
        assert!(closed.starts_with("HTTP/1.1 204"), "{closed}");
        assert_eq!(command(&path, "quiesce").await["accepted"], true);
        let mut idle = tokio::net::TcpStream::connect(address).await.unwrap();
        idle.write_all(b"GET /").await.unwrap();
        let release_path = path.clone();
        let mut release = tokio::spawn(async move { command(&release_path, "release").await });
        stopped.changed().await.unwrap();
        assert!(*stopped.borrow());
        tokio::time::timeout(Duration::from_secs(3), http_server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(idle.read(&mut [0_u8; 1]).await.unwrap(), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut release)
                .await
                .is_err()
        );
        let database = state.database().await.unwrap().clone();
        database
            .execute_unprepared(
                "CREATE TABLE proof (answer INTEGER); INSERT INTO proof VALUES (42);",
            )
            .await
            .unwrap();
        close_database(&state).await.unwrap();
        assert!(database.execute_unprepared("SELECT 1;").await.is_err());
        let reopened = sea_orm::Database::connect(&config.database.url)
            .await
            .unwrap();
        let row = reopened
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT answer FROM proof;".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by_index::<i32>(0).unwrap(), 42);
        reopened.close().await.unwrap();
        tracker().released();
        finished.send(true).unwrap();
        let released = release.await.unwrap();
        assert_eq!(released["accepted"], true);
        assert_eq!(released["status"]["phase"], "released");
        manager.await.unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires WRIX_TEST_CELLERD naming the built patched native daemon"]
    async fn native_daemon_exit_and_authenticated_next_owner_preserve_signing_state() {
        let binary = std::env::var_os("WRIX_TEST_CELLERD").expect("patched native daemon required");
        let directory =
            std::env::temp_dir().join(format!("celler-native-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let secret = b"native-owner-fixture-not-a-live-key";
        std::fs::write(directory.join("jwt.key"), secret).unwrap();
        let socket = directory.join("drain.sock");
        let config = directory.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
[database]
url = "sqlite://{}?mode=rwc"
heartbeat = true
[storage]
type = "local"
path = "{}"
[chunking]
nar-size-threshold = 1
min-size = 64
avg-size = 256
max-size = 1024
[jwt]
hs256-secret-key-file = "{}"
"#,
                directory.join("metadata.db").display(),
                directory.join("archive").display(),
                directory.join("jwt.key").display()
            ),
        )
        .unwrap();
        let signing = attic_token::SignatureType::HS256(attic_token::HS256Key::from_bytes(secret));
        let mut permission = attic_token::Token::new(
            "fixture".into(),
            &(chrono::Utc::now() + chrono::Duration::minutes(5)),
        );
        let grant = permission
            .get_or_insert_permission_mut(CacheName::new("private".into()).unwrap().to_pattern());
        grant.pull = true;
        grant.push = true;
        grant.create_cache = true;
        let token = permission.encode(&signing, &None, &None).unwrap();
        let mut retained_key = None;
        for owner in 0..2 {
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = reservation.local_addr().unwrap();
            drop(reservation);
            let mut daemon = tokio::process::Command::new(&binary)
                .arg("--config")
                .arg(&config)
                .arg("--listen")
                .arg(address.to_string())
                .env("WRIX_CELLER_DRAIN_SOCKET", &socket)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let started = tokio::time::Instant::now();
            while !socket.exists() {
                assert!(daemon.try_wait().unwrap().is_none());
                assert!(started.elapsed() < Duration::from_secs(10));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let private = tokio::process::Command::new(&binary)
                .args(["--work-drain", "status"])
                .env("WRIX_CELLER_DRAIN_SOCKET", &socket)
                .output()
                .await
                .unwrap();
            assert!(private.status.success());
            let status: serde_json::Value = serde_json::from_slice(&private.stdout).unwrap();
            assert_eq!(status["status"]["phase"], "serving");
            if owner == 0 {
                let admitted =
                    http(address, "POST", "/_api/v1/workflow/private", &token, None).await;
                assert!(admitted.starts_with("HTTP/1.1 200"), "{admitted}");
                let body: serde_json::Value = serde_json::from_str(
                    &admitted[admitted.find('{').unwrap()..=admitted.rfind('}').unwrap()],
                )
                .unwrap();
                let workflow = body["workflow"].as_str().unwrap();
                let payload =
                    serde_json::to_vec(&attic::api::v1::cache_config::CreateCacheRequest {
                        keypair: attic::api::v1::cache_config::KeypairConfig::Generate,
                        is_public: false,
                        store_dir: "/nix/store".into(),
                        priority: 40,
                        upstream_cache_key_names: vec![],
                    })
                    .unwrap();
                let created = http_body(
                    address,
                    "POST",
                    "/_api/v1/cache-config/private",
                    &token,
                    Some(workflow),
                    "Content-Type: application/json\r\n",
                    &payload,
                )
                .await;
                assert!(created.starts_with("HTTP/1.1 200"), "{created}");
                assert!(
                    http(
                        address,
                        "DELETE",
                        "/_api/v1/workflow/private",
                        &token,
                        Some(workflow)
                    )
                    .await
                    .starts_with("HTTP/1.1 204")
                );
            }
            let ready = http(
                address,
                "GET",
                "/_api/v1/cache-config/private",
                &token,
                None,
            )
            .await;
            assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");
            let body: serde_json::Value =
                serde_json::from_str(&ready[ready.find('{').unwrap()..=ready.rfind('}').unwrap()])
                    .unwrap();
            let key = body["public_key"].as_str().unwrap().to_owned();
            if let Some(retained) = &retained_key {
                assert_eq!(&key, retained);
            } else {
                retained_key = Some(key);
            }
            invoke_at(&socket, Command::Begin).await.unwrap();
            let draining = tokio::time::Instant::now();
            loop {
                let status: serde_json::Value =
                    serde_json::from_slice(&invoke_at(&socket, Command::Status).await.unwrap())
                        .unwrap();
                assert_eq!(status["status"]["failed"], false);
                if status["status"]["requests"] == 0
                    && status["status"]["workflows"] == 0
                    && status["status"]["background"] == 0
                {
                    break;
                }
                assert!(draining.elapsed() < Duration::from_secs(5));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            invoke_at(&socket, Command::Quiesce).await.unwrap();
            let released: serde_json::Value =
                serde_json::from_slice(&invoke_at(&socket, Command::Release).await.unwrap())
                    .unwrap();
            assert_eq!(released["status"]["phase"], "released");
            assert!(
                tokio::time::timeout(Duration::from_secs(5), daemon.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
            assert!(!socket.exists());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
