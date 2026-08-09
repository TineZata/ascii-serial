use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_serial::SerialPortBuilderExt;

pub mod client;
pub use client::{ClientConfig, Context};

#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port_path: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub open_timeout: Duration,
    pub io_timeout: Duration,
    pub line_ending: LineEnding,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataBits {
    Five,
    Six,
    Seven,
    Eight,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StopBits {
    One,
    Two,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port_path: "COM1".to_string(),
            baud_rate: 9600,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            open_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_secs(2),
            line_ending: LineEnding::Lf,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Debug, Error)]
pub enum AsciiSerialError {
    #[error("open timeout")]
    OpenTimeout,
    #[error("I/O timeout")]
    IoTimeout,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection closed before response")]
    Eof,
    #[error("port is already in use: {0}")]
    PortBusy(String),
    #[error("invalid protocol type: {0}")]
    InvalidProtocolType(String),
    #[error("backend config parse error: {0}")]
    ConfigParse(#[from] serde_json::Error),
}

impl AsciiSerialError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::OpenTimeout | Self::IoTimeout | Self::Io(_) | Self::Eof | Self::PortBusy(_)
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HardwareProtocol {
    #[serde(rename = "type")]
    pub kind: String,
    pub port_path: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub line_ending: LineEnding,
    pub open_timeout_ms: Option<u64>,
    pub io_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Command {
    pub id: Option<String>,
    pub request: String,
    pub protocol: HardwareProtocol,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandResult {
    pub id: Option<String>,
    pub response: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchCommands {
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchResult {
    pub responses: Vec<CommandResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Payload {
    Single(Command),
    Batch(BatchCommands),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandResultPayload {
    Single(CommandResult),
    Batch(BatchResult),
}

impl SerialConfig {
    pub fn from_protocol(protocol: &HardwareProtocol) -> Result<Self, AsciiSerialError> {
        if protocol.kind != "ascii-serial" {
            return Err(AsciiSerialError::InvalidProtocolType(protocol.kind.clone()));
        }

        Ok(Self {
            port_path: protocol.port_path.clone(),
            baud_rate: protocol.baud_rate,
            data_bits: protocol.data_bits,
            parity: protocol.parity,
            stop_bits: protocol.stop_bits,
            open_timeout: Duration::from_millis(protocol.open_timeout_ms.unwrap_or(2_000)),
            io_timeout: Duration::from_millis(protocol.io_timeout_ms.unwrap_or(2_000)),
            line_ending: protocol.line_ending,
        })
    }
}

pub fn parse_backend_command(json: &str) -> Result<Command, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

pub fn parse_backend_batch(json: &str) -> Result<BatchCommands, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

pub fn parse_backend_payload(json: &str) -> Result<Payload, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

#[derive(Debug, Default)]
pub(crate) struct SerialBackend {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SerialBackend {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn exchange_line(
        &self,
        cfg: &SerialConfig,
        request: &str,
    ) -> Result<String, AsciiSerialError> {
        let _guard = self.acquire_port(cfg).await;
        exchange_line(cfg, request).await
    }

    pub(crate) async fn execute_command(
        &self,
        cmd: &Command,
    ) -> Result<CommandResult, AsciiSerialError> {
        let cfg = SerialConfig::from_protocol(&cmd.protocol)?;
        let response = self.exchange_line(&cfg, &cmd.request).await?;
        Ok(CommandResult {
            id: cmd.id.clone(),
            response,
        })
    }

    async fn acquire_port(&self, cfg: &SerialConfig) -> OwnedMutexGuard<()> {
        let lock = self.get_or_create_lock(&cfg.port_path).await;
        lock.lock_owned().await
    }

    async fn get_or_create_lock(&self, port_path: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(port_path.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub async fn exchange_line(
    cfg: &SerialConfig,
    request: &str,
) -> Result<String, AsciiSerialError> {
    let stream = tokio::time::timeout(cfg.open_timeout, open_stream(cfg))
        .await
        .map_err(|_| AsciiSerialError::OpenTimeout)??;

    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    let outbound = format!("{}{}", request, cfg.line_ending.as_str());
    tokio::time::timeout(cfg.io_timeout, async {
        writer.write_all(outbound.as_bytes()).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| AsciiSerialError::IoTimeout)??;

    let mut line = String::new();
    let read_n = tokio::time::timeout(cfg.io_timeout, reader.read_line(&mut line))
        .await
        .map_err(|_| AsciiSerialError::IoTimeout)??;

    if read_n == 0 {
        return Err(AsciiSerialError::Eof);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(line)
}

async fn open_stream(cfg: &SerialConfig) -> Result<tokio_serial::SerialStream, std::io::Error> {
    let data_bits = match cfg.data_bits {
        DataBits::Five => tokio_serial::DataBits::Five,
        DataBits::Six => tokio_serial::DataBits::Six,
        DataBits::Seven => tokio_serial::DataBits::Seven,
        DataBits::Eight => tokio_serial::DataBits::Eight,
    };
    let parity = match cfg.parity {
        Parity::None => tokio_serial::Parity::None,
        Parity::Odd => tokio_serial::Parity::Odd,
        Parity::Even => tokio_serial::Parity::Even,
    };
    let stop_bits = match cfg.stop_bits {
        StopBits::One => tokio_serial::StopBits::One,
        StopBits::Two => tokio_serial::StopBits::Two,
    };

    let builder = tokio_serial::new(&cfg.port_path, cfg.baud_rate)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits);
    builder
        .open_native_async()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}
