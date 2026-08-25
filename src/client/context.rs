use crate::{
    parse_backend_payload, Payload, CommandResultPayload, AsciiSerialError,
};

use super::serial::ContextInner;
use super::ClientConfig;

/// Manages command execution against one or more serial ports with optional retry and connect/disconnect callbacks.
#[derive(Clone, Default)]
pub struct Context {
    inner: ContextInner,
    config: ClientConfig,
}

impl Context {
    /// Creates a new `Context` with configuration loaded from environment variables.
    ///
    /// Reads `ASCII_SERIAL_RETRY_MS` and `ASCII_SERIAL_RETRY_FOREVER`; see [`ClientConfig::from_env`].
    pub fn from_env() -> Self {
        Self {
            inner: ContextInner::new(),
            config: ClientConfig::from_env(),
        }
    }

    /// Creates a new `Context` with the provided `ClientConfig`.
    pub fn new(config: ClientConfig) -> Self {
        Self {
            inner: ContextInner::new(),
            config,
        }
    }

    /// Registers a callback invoked once when a port transitions from unknown/disconnected to connected.
    pub fn on_connected<F>(&self, callback: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        self.inner.on_connected(callback);
    }

    /// Registers a callback invoked on every failed attempt, with the error's `Display` text as the reason.
    pub fn on_disconnected<F>(&self, callback: F)
    where
        F: Fn(String, String) + Send + Sync + 'static,
    {
        self.inner.on_disconnected(callback);
    }

    /// Executes a payload, retrying on transient errors if `retry_forever` is `true` in [`ClientConfig`].
    pub async fn execute_payload(
        &self,
        payload: &Payload,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        if self.config.retry_forever {
            self.inner
                .execute_payload_retrying(payload, self.config.retry_interval)
                .await
        } else {
            self.inner.execute_payload(payload).await
        }
    }

    /// Executes the payload once without retrying, returning the first error encountered.
    pub async fn execute_payload_once(
        &self,
        payload: &Payload,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        self.inner.execute_payload(payload).await
    }

    /// Parses `json` as a payload and executes it, retrying according to [`ClientConfig`].
    pub async fn execute_json(
        &self,
        json: &str,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        let payload = parse_backend_payload(json)?;
        self.execute_payload(&payload).await
    }

    /// Returns the [`ClientConfig`] this context was created with.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }
}
