use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{
    AsciiSerialError, BatchCommands, BatchResult, Command, CommandResult, CommandResultPayload,
    Payload, SerialBackend,
};

type ConnectedCallback = Arc<dyn Fn(String) + Send + Sync>;
type DisconnectedCallback = Arc<dyn Fn(String, String) + Send + Sync>;

/// Connection state and callbacks shared by every clone of a [`Context`](super::Context).
///
/// Connection state is tracked per port path; connected callbacks fire on state transitions only.
#[derive(Clone)]
pub(crate) struct ContextInner {
    backend: Arc<SerialBackend>,
    connection_state: Arc<std::sync::Mutex<HashMap<String, bool>>>,
    on_connected_callbacks: Arc<std::sync::Mutex<Vec<ConnectedCallback>>>,
    on_disconnected_callbacks: Arc<std::sync::Mutex<Vec<DisconnectedCallback>>>,
}

impl ContextInner {
    pub(crate) fn new() -> Self {
        Self {
            backend: Arc::new(SerialBackend::new()),
            connection_state: Arc::new(std::sync::Mutex::new(HashMap::new())),
            on_connected_callbacks: Arc::new(std::sync::Mutex::new(Vec::new())),
            on_disconnected_callbacks: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Registers a callback invoked with the port path when it first becomes reachable (or recovers).
    pub(crate) fn on_connected<F>(&self, callback: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        if let Ok(mut callbacks) = self.on_connected_callbacks.lock() {
            callbacks.push(Arc::new(callback));
        }
    }

    /// Registers a callback invoked with `(port_path, reason)` on every connection-level failure.
    pub(crate) fn on_disconnected<F>(&self, callback: F)
    where
        F: Fn(String, String) + Send + Sync + 'static,
    {
        if let Ok(mut callbacks) = self.on_disconnected_callbacks.lock() {
            callbacks.push(Arc::new(callback));
        }
    }

    /// Executes one command, accepting the first response line.
    pub(crate) async fn execute_command(
        &self,
        cmd: &Command,
    ) -> Result<CommandResult, AsciiSerialError> {
        self.execute_command_matching(cmd, |_| true).await
    }

    /// Executes one command and updates connection state: success marks the port connected,
    /// failure is recorded via `record_failure`.
    pub(crate) async fn execute_command_matching<F>(
        &self,
        cmd: &Command,
        accept: F,
    ) -> Result<CommandResult, AsciiSerialError>
    where
        F: Fn(&str) -> bool,
    {
        let port_path = &cmd.protocol.port_path;
        match self.backend.execute_command_matching(cmd, accept).await {
            Ok(result) => {
                self.mark_connected(port_path);
                Ok(result)
            }
            Err(err) => {
                self.record_failure(port_path, &cmd.request, &err);
                Err(err)
            }
        }
    }

    /// Sends one command without waiting for a reply, updating connection state like `execute_command_matching`.
    pub(crate) async fn send_command(&self, cmd: &Command) -> Result<(), AsciiSerialError> {
        let port_path = &cmd.protocol.port_path;
        match self.backend.send_command(cmd).await {
            Ok(()) => {
                self.mark_connected(port_path);
                Ok(())
            }
            Err(err) => {
                self.record_failure(port_path, &cmd.request, &err);
                Err(err)
            }
        }
    }

    /// Executes a single command or a batch once, failing on the first error.
    pub(crate) async fn execute_payload(
        &self,
        payload: &Payload,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        execute_payload_with(payload, |cmd| async move { self.execute_command(&cmd).await }).await
    }

    /// Like `execute_payload`, but sleeps `retry_interval` and starts over on retryable errors.
    pub(crate) async fn execute_payload_retrying(
        &self,
        payload: &Payload,
        retry_interval: Duration,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        retry_with_interval("execute_payload", retry_interval, || {
            self.execute_payload(payload)
        })
        .await
    }

    /// Flips the port to connected and fires the callbacks only on a disconnected → connected transition.
    fn mark_connected(&self, port_path: &str) {
        let mut should_emit = false;
        if let Ok(mut state) = self.connection_state.lock() {
            let was_connected = state.get(port_path).copied().unwrap_or(false);
            if !was_connected {
                state.insert(port_path.to_string(), true);
                should_emit = true;
            }
        }
        if should_emit {
            info!(port = %port_path, "serial connection established");
            self.emit_connected(port_path.to_string());
        }
    }

    /// Records a failed request. A reply timeout says nothing about the connection (the device
    /// may simply ignore that command), so only connection-level errors count as a disconnect.
    fn record_failure(&self, port_path: &str, request: &str, err: &AsciiSerialError) {
        if matches!(err, AsciiSerialError::IoTimeout) {
            warn!(port = %port_path, request, "no matching reply before I/O timeout");
        } else {
            self.mark_disconnected(port_path, err);
        }
    }

    /// Flips the port to disconnected and fires the callbacks on every call, so each failed attempt is reported.
    fn mark_disconnected(&self, port_path: &str, err: &AsciiSerialError) {
        if let Ok(mut state) = self.connection_state.lock() {
            state.insert(port_path.to_string(), false);
        }
        warn!(port = %port_path, error = %err, "serial connection attempt failed");
        self.emit_disconnected(port_path.to_string(), err.to_string());
    }

    fn emit_connected(&self, port_path: String) {
        let callbacks = if let Ok(callbacks) = self.on_connected_callbacks.lock() {
            callbacks.clone()
        } else {
            debug!("connected callback lock poisoned; skipping callbacks");
            Vec::new()
        };
        for cb in callbacks {
            cb(port_path.clone());
        }
    }

    fn emit_disconnected(&self, port_path: String, reason: String) {
        let callbacks = if let Ok(callbacks) = self.on_disconnected_callbacks.lock() {
            callbacks.clone()
        } else {
            debug!("disconnected callback lock poisoned; skipping callbacks");
            Vec::new()
        };
        for cb in callbacks {
            cb(port_path.clone(), reason.clone());
        }
    }
}

impl Default for ContextInner {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs every command of `batch` in order, stopping at the first error.
async fn execute_batch_with<F, Fut>(
    batch: &BatchCommands,
    mut execute: F,
) -> Result<BatchResult, AsciiSerialError>
where
    F: FnMut(Command) -> Fut,
    Fut: Future<Output = Result<CommandResult, AsciiSerialError>>,
{
    let mut responses = Vec::with_capacity(batch.commands.len());
    for cmd in &batch.commands {
        responses.push(execute(cmd.clone()).await?);
    }
    Ok(BatchResult { responses })
}

/// Dispatches `payload` to `execute`, preserving its single/batch shape in the result.
async fn execute_payload_with<F, Fut>(
    payload: &Payload,
    mut execute: F,
) -> Result<CommandResultPayload, AsciiSerialError>
where
    F: FnMut(Command) -> Fut,
    Fut: Future<Output = Result<CommandResult, AsciiSerialError>>,
{
    match payload {
        Payload::Single(cmd) => {
            Ok(CommandResultPayload::Single(execute(cmd.clone()).await?))
        }
        Payload::Batch(batch) => Ok(CommandResultPayload::Batch(
            execute_batch_with(batch, execute).await?,
        )),
    }
}

/// Repeats `execute` until it succeeds or fails with a non-retryable error, sleeping `retry_interval` between attempts.
async fn retry_with_interval<T, F, Fut>(
    operation: &str,
    retry_interval: Duration,
    mut execute: F,
) -> Result<T, AsciiSerialError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AsciiSerialError>>,
{
    loop {
        match execute().await {
            Ok(result) => return Ok(result),
            Err(err) if err.is_retryable() => {
                warn!(
                    operation = operation,
                    error = %err,
                    retry_ms = retry_interval.as_millis(),
                    "retryable execution error"
                );
                tokio::time::sleep(retry_interval).await;
            }
            Err(err) => return Err(err),
        }
    }
}
