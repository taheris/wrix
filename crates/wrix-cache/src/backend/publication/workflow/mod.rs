//! Native publication admission spanning every request and idle HTTP gap.
pub mod realization;
use super::super::{Client, Error, Result, success};
use futures::StreamExt;
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use std::time::Duration;

/// Cancellation deliberately retains native admission until explicitly reconciled.
pub struct Session {
    client: Client,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Opened {
    workflow: String,
}

impl Session {
    pub async fn open(client: &Client) -> Result<Self> {
        let response = success(
            client
                .request(
                    Method::POST,
                    &format!("_api/v1/workflow/{}", client.namespace),
                )?
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        if response
            .content_length()
            .is_some_and(|length| length > 1024)
        {
            return Err(Error::Protocol);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Transport)?;
            if bytes.len() + chunk.len() > 1024 {
                return Err(Error::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let opened: Opened = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        if opened.workflow.len() != 36
            || !opened
                .workflow
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(Error::Protocol);
        }
        let mut headers = HeaderMap::new();
        let mut id = HeaderValue::from_str(&opened.workflow).map_err(|_| Error::Protocol)?;
        id.set_sensitive(true);
        headers.insert("x-wrix-workflow", id);
        let mut client = client.clone();
        client.http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self { client })
    }

    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// Ending admission is not evidence that publication itself succeeded.
    pub async fn finish(self) -> Result<()> {
        success(
            self.client
                .request(
                    Method::DELETE,
                    &format!("_api/v1/workflow/{}", self.client.namespace),
                )?
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )?;
        Ok(())
    }
}
