//! Minimal client for Herdr's one-request-per-connection Unix socket API.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct ApiError(String);

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ApiError {}

#[derive(Clone)]
pub struct ApiClient {
    socket_path: PathBuf,
}

impl ApiClient {
    /// Verify the local socket with a ping before returning a reusable client.
    pub async fn connect(socket_path: &Path) -> Result<Self, ApiError> {
        let client = Self {
            socket_path: socket_path.to_path_buf(),
        };
        client.request("ping", json!({})).await?;
        Ok(client)
    }

    /// Send one JSON request on a fresh connection and return its result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        timeout(REQUEST_TIMEOUT, self.request_inner(method, params))
            .await
            .map_err(|_| ApiError(format!("api timeout: {method}")))?
    }

    async fn request_inner(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        let stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket_path))
            .await
            .map_err(|_| {
                ApiError(format!(
                    "api connect timeout: {}",
                    self.socket_path.display()
                ))
            })?
            .map_err(|error| {
                ApiError(format!(
                    "could not connect to Herdr socket {}: {error}",
                    self.socket_path.display()
                ))
            })?;
        let (read, mut write) = stream.into_split();
        let id = format!(
            "herdr_desktop_switcher_{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let line = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|error| ApiError(format!("could not encode {method} request: {error}")))?
            + "\n";
        write
            .write_all(line.as_bytes())
            .await
            .map_err(|error| ApiError(format!("could not send {method} request: {error}")))?;

        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| ApiError(format!("could not read {method} response: {error}")))?
        {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .or_else(|| error.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string());
                return Err(ApiError(format!("{method}: {text}")));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(ApiError(format!("api closed before response: {method}")))
    }
}
