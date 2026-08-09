use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{
    BatchCommands, BatchResult, Command,
    Payload, CommandResultPayload, CommandResult,
    AsciiSerialError, SerialBackend,
};

type ConnectedCallback = Arc<dyn Fn(String) + Send + Sync>;
type DisconnectedCallback = Arc<dyn Fn(String, String) + Send + Sync>;

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

    pub(crate) fn on_connected<F>(&self, callback: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        if let Ok(mut callbacks) = self.on_connected_callbacks.lock() {
            callbacks.push(Arc::new(callback));
        }
    }

    pub(crate) fn on_disconnected<F>(&self, callback: F)
    where
        F: Fn(String, String) + Send + Sync + 'static,
    {
        if let Ok(mut callbacks) = self.on_disconnected_callbacks.lock() {
            callbacks.push(Arc::new(callback));
        }
    }

    pub(crate) async fn execute_command(
        &self,
        cmd: &Command,
    ) -> Result<CommandResult, AsciiSerialError> {
        let port_path = cmd.protocol.port_path.clone();
        match self.backend.execute_command(cmd).await {
            Ok(result) => {
                self.mark_connected(&port_path);
                Ok(result)
            }
            Err(err) => {
                self.mark_disconnected(&port_path, &err);
                Err(err)
            }
        }
    }

    pub(crate) async fn execute_payload(
        &self,
        payload: &Payload,
    ) -> Result<CommandResultPayload, AsciiSerialError> {
        execute_payload_with(payload, |cmd| async move { self.execute_command(&cmd).await }).await
    }

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

    fn mark_disconnected(&self, port_path: &str, err: &AsciiSerialError) {
        let mut should_emit = false;
        if let Ok(mut state) = self.connection_state.lock() {
            let was_connected = state.get(port_path).copied();
            if was_connected.unwrap_or(true) {
                state.insert(port_path.to_string(), false);
                should_emit = true;
            }
        }
        if should_emit {
            warn!(port = %port_path, error = %err, "serial connection lost");
            self.emit_disconnected(port_path.to_string(), err.to_string());
        }
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
