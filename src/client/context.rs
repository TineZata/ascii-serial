use crate::{
    parse_backend_payload, AsciiSerialError, Command, CommandResult, CommandResultPayload, Payload,
};

use super::context_inner::ContextInner;
use super::ClientConfig;

/// Manages command execution against one or more serial ports with optional retry and connect/disconnect callbacks.
///
/// Each port path gets a single persistent connection, opened on first use and shared by every
/// request (and clones of the `Context`) until it fails, after which the next request reopens it.
/// Requests to the same port are queued, not rejected. A command with an empty `request` sends
/// nothing and only waits for a matching incoming line, which suits unsolicited heartbeats.
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

    /// Registers a callback invoked with `(port_path, reason)` on every failed attempt.
    ///
    /// `reason` is the error's `Display` text. A reply timeout does not count as a disconnect.
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

    /// Executes one command without retrying, skipping response lines `accept` rejects.
    ///
    /// Use when the device interleaves unsolicited lines (banners, heartbeats) with replies.
    pub async fn execute_command_matching<F>(
        &self,
        command: &Command,
        accept: F,
    ) -> Result<CommandResult, AsciiSerialError>
    where
        F: Fn(&str) -> bool,
    {
        self.inner.execute_command_matching(command, accept).await
    }

    /// Sends one command over the shared connection without waiting for a reply.
    pub async fn send_command(&self, command: &Command) -> Result<(), AsciiSerialError> {
        self.inner.send_command(command).await
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
