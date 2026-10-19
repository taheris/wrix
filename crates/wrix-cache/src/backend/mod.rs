//! Scoped host publication through Celler; no sandbox or host substitution authority.

pub mod issuer;
pub mod layout;
pub mod publication;

use std::{collections::HashSet, path::PathBuf, time::Duration};

use attic::{
    api::v1::{
        cache_config::{CacheConfig, CreateCacheRequest, KeypairConfig, RetentionPeriodConfig},
        upload_path::{CELLER_NAR_INFO_PREAMBLE_SIZE, UploadPathNarInfo},
    },
    cache::CacheName,
    nix_store::NixStore,
};
use bytes::Bytes;
use displaydoc::Display;
use futures::{Stream, StreamExt, stream};
use reqwest::{Client as HttpClient, StatusCode, Url, header::HeaderValue};
use thiserror::Error as ThisError;
use wrix_core::cache_key::CachePublicKey;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// invalid Celler endpoint; expected a loopback HTTP origin
    Endpoint,
    /// invalid publishing credential
    Credential,
    /// Celler transport failed
    Transport,
    /// Celler returned HTTP {0}
    Status(StatusCode),
    /// invalid Celler protocol response
    Protocol,
    /// host store operation failed: {0}
    Store(#[from] attic::AtticError),
    /// invalid Celler public signing key
    PublicKey,
    /// retention must be positive and representable by Celler
    Retention,
    /// NAR size exceeds the native upload range
    NarSize,
    /// Celler changed a valid signing key during registration
    KeyChanged,
    /// Celler invalidated the selected view, but replacement key/policy confirmation failed; republish only after recovery: {source}
    RotationInvalidated { source: Box<Error> },
}

/// One host-authorized namespace and its runtime HTTP authority.
#[derive(Clone)]
pub struct Client {
    endpoint: Url,
    namespace: CacheName,
    http: HttpClient,
    authorization: HeaderValue,
}

impl Client {
    /// Credentials must come from host-private runtime state, never repository policy.
    pub fn new(endpoint: &str, namespace: CacheName, token: &str) -> Result<Self> {
        let endpoint = Url::parse(endpoint).map_err(|_| Error::Endpoint)?;
        let loopback = endpoint
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "[::1]" || host == "::1");
        if endpoint.scheme() != "http"
            || !loopback
            || endpoint.path() != "/"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error::Endpoint);
        }
        if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(Error::Credential);
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Credential)?;
        authorization.set_sensitive(true);
        let http = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            endpoint,
            namespace,
            http,
            authorization,
        })
    }

    fn request(&self, method: reqwest::Method, route: &str) -> Result<reqwest::RequestBuilder> {
        let endpoint = self.endpoint.join(route).map_err(|_| Error::Endpoint)?;
        Ok(self
            .http
            .request(method, endpoint)
            .header(reqwest::header::AUTHORIZATION, self.authorization.clone()))
    }

    fn config_route(&self) -> String {
        format!("_api/v1/cache-config/{}", self.namespace)
    }

    async fn configuration(&self) -> Result<Option<CacheConfig>> {
        let response = self
            .request(reqwest::Method::GET, &self.config_route())?
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = success(response)?;
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Transport)?;
            if bytes.len() + chunk.len() > 16 * 1024 {
                return Err(Error::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::Protocol)
    }

    /// Preserves a valid existing key and confirms authenticated finite retention.
    pub async fn register(&self, retention: Duration) -> Result<CachePublicKey> {
        let seconds = retention_seconds(retention)?;
        let prior_key = match self.configuration().await {
            Ok(Some(config)) => Some(match parse_public_key(&config) {
                Ok(key) => key,
                Err(Error::PublicKey) => self.repair_key().await?,
                Err(error) => return Err(error),
            }),
            Ok(None) => {
                self.create().await?;
                None
            }
            Err(Error::Status(StatusCode::INTERNAL_SERVER_ERROR)) => Some(self.repair_key().await?),
            Err(error) => return Err(error),
        };
        let mut config = CacheConfig::blank();
        config.is_public = Some(false);
        config.retention_period = Some(RetentionPeriodConfig::Period(seconds));
        self.configure(&config).await?;
        let confirmed = self.configuration().await?.ok_or(Error::Protocol)?;
        if confirmed.is_public != Some(false)
            || !matches!(confirmed.retention_period, Some(RetentionPeriodConfig::Period(value)) if value == seconds)
        {
            return Err(Error::Protocol);
        }
        let key = parse_public_key(&confirmed)?;
        if prior_key.is_some_and(|prior| prior != key) {
            return Err(Error::KeyChanged);
        }
        Ok(key)
    }

    /// Native conditional repair never replaces a valid key, even after an inconclusive GET.
    async fn repair_key(&self) -> Result<CachePublicKey> {
        success(
            self.request(reqwest::Method::PATCH, &self.config_route())?
                .json(&serde_json::json!({"keypair": "GenerateIfInvalid"}))
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        self.public_key().await
    }

    async fn create(&self) -> Result<()> {
        let payload = CreateCacheRequest {
            keypair: KeypairConfig::Generate,
            is_public: false,
            store_dir: String::from("/nix/store"),
            priority: 40,
            upstream_cache_key_names: Vec::new(),
        };
        success(
            self.request(reqwest::Method::POST, &self.config_route())?
                .json(&payload)
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        Ok(())
    }

    async fn configure(&self, payload: &CacheConfig) -> Result<()> {
        success(
            self.request(reqwest::Method::PATCH, &self.config_route())?
                .json(payload)
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        Ok(())
    }

    pub async fn public_key(&self) -> Result<CachePublicKey> {
        let config = self.configuration().await?.ok_or(Error::Protocol)?;
        parse_public_key(&config)
    }

    /// Invalidates only this namespace; callers hold its publication lock.
    pub async fn rotate(&self, retention: Duration) -> Result<CachePublicKey> {
        retention_seconds(retention)?;
        success(
            self.request(reqwest::Method::DELETE, &self.config_route())?
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        self.register(retention)
            .await
            .map_err(|source| Error::RotationInvalidated {
                source: Box::new(source),
            })
    }

    /// Uploads precisely the admitted realized paths, without following references.
    pub async fn publish_selected(&self, paths: &[PathBuf]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let store = Box::pin(NixStore::connect()).await?;
        let mut published = 0;
        let mut seen = HashSet::new();
        for path in paths {
            if !seen.insert(path) {
                continue;
            }
            let parsed = store.parse_store_path(path)?;
            if store.get_full_path(&parsed) != *path {
                return Err(Error::Protocol);
            }
            let Some(info) = store.query_path_info(&parsed).await? else {
                continue;
            };
            let metadata = UploadPathNarInfo {
                cache: self.namespace.clone(),
                store_path_hash: parsed.to_hash(),
                store_path: path.to_string_lossy().into_owned(),
                references: info
                    .references
                    .iter()
                    .map(|path| path.as_os_str().to_string_lossy().into_owned())
                    .collect(),
                system: None,
                deriver: None,
                sigs: Vec::new(),
                ca: info.ca,
                nar_hash: info.nar_hash,
                nar_size: usize::try_from(info.nar_size).map_err(|_| Error::NarSize)?,
            };
            let nar = store.nar_from_path(parsed);
            self.upload(metadata, nar).await?;
            published += 1;
        }
        Ok(published)
    }

    async fn upload<S>(&self, metadata: UploadPathNarInfo, nar: S) -> Result<()>
    where
        S: Stream<Item = attic::AtticResult<Bytes>> + Send + 'static,
    {
        let preamble = serde_json::to_vec(&metadata).map_err(|_| Error::Protocol)?;
        let length = preamble.len();
        let body = stream::once(async move { Ok::<_, attic::AtticError>(Bytes::from(preamble)) })
            .chain(nar);
        success(
            self.request(reqwest::Method::PUT, "_api/v1/upload-path")?
                .header(CELLER_NAR_INFO_PREAMBLE_SIZE, length)
                .body(reqwest::Body::wrap_stream(body))
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        Ok(())
    }
}

/// Reports missing selected outputs without converting native store failures into misses.
pub async fn paths_realized(paths: &[String]) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    let store = Box::pin(NixStore::connect()).await?;
    for path in paths {
        let parsed = store.parse_store_path(path)?;
        if store.get_full_path(&parsed) != *path {
            return Err(Error::Protocol);
        }
        if store.query_path_info(&parsed).await?.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Returns only realized runtime references of the selected outputs; unknown store errors propagate.
pub async fn realized_closure(paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let store = Box::pin(NixStore::connect()).await?;
    let mut realized = Vec::new();
    for path in paths {
        let parsed = store.parse_store_path(path)?;
        if store.get_full_path(&parsed) != *path {
            return Err(Error::Protocol);
        }
        if store.query_path_info(&parsed).await?.is_some() {
            realized.push(parsed);
        }
    }
    Ok(store
        .compute_fs_closure_multi(realized, false)
        .await?
        .into_iter()
        .map(|path| store.get_full_path(path).to_string_lossy().into_owned())
        .collect())
}

fn parse_public_key(config: &CacheConfig) -> Result<CachePublicKey> {
    let key = config.public_key.as_deref().ok_or(Error::PublicKey)?;
    CachePublicKey::parse(key).map_err(|_| Error::PublicKey)
}

fn success(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(Error::Status(response.status()))
    }
}

fn retention_seconds(duration: Duration) -> Result<u32> {
    if duration.is_zero() || duration.subsec_nanos() != 0 {
        return Err(Error::Retention);
    }
    let seconds = u32::try_from(duration.as_secs()).map_err(|_| Error::Retention)?;
    if seconds > i32::MAX.cast_unsigned() {
        return Err(Error::Retention);
    }
    Ok(seconds)
}
