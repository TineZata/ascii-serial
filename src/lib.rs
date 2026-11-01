//! Tokio-based ASCII line request/response client over serial ports.
//!
//! * [`Context`] is the high-level client: one persistent connection per port path,
//!   connect/disconnect callbacks and optional retry.
//! * [`ChannelConfig`] / [`ChannelReader`] describe one device value (poll command, `read_response`
//!   template, scale/offset); [`Context::read_channel`] and [`Context::write_channel`] apply them.
//! * [`exchange_line`] / [`exchange_line_matching`] / [`send_line`] are one-shot helpers that
//!   open the port for a single request.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_serial::SerialPortBuilderExt;

use crate::client::endpoint::Endpoint;

pub mod client;
pub mod template;
pub use client::{
    AsciiResponseMode, ChannelConfig, ChannelReader, ClientConfig, Context, Reading,
};
pub use template::{Capture, ResponseTemplate, SpecKind};

/// Connection parameters for a single ASCII serial port.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    /// Port name, e.g. `COM3` or `/dev/ttyUSB0`.
    pub port_path: String,
    /// Baud rate.
    pub baud_rate: u32,
    /// Data bits per frame.
    pub data_bits: DataBits,
    /// Parity setting.
    pub parity: Parity,
    /// Stop bits per frame.
    pub stop_bits: StopBits,
    /// Maximum time to wait for the port to open
    /// before failing with [`AsciiSerialError::OpenTimeout`].
    pub open_timeout: Duration,
    /// Maximum time to wait for a single write or reply once open
    /// before failing with [`AsciiSerialError::IoTimeout`].
    pub io_timeout: Duration,
    /// Terminator appended to requests and expected at the end of responses.
    pub line_ending: LineEnding,
}

/// Number of data bits per serial frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataBits {
    Five,
    Six,
    Seven,
    Eight,
}

/// Parity bit setting for the serial port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    None,
    Odd,
    Even,
}

/// Number of stop bits per serial frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StopBits {
    One,
    Two,
}

impl Default for SerialConfig {
    /// `COM1`, 9600 8N1, 2 s open and I/O timeouts, LF line ending.
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

/// Line ending used to frame requests and responses.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    /// Line feed (`\n`).
    #[default]
    Lf,
    /// Carriage return followed by line feed (`\r\n`).
    CrLf,
    /// Carriage return (`\r`).
    Cr,
}

impl LineEnding {
    /// The terminator as it appears on the wire.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

/// Errors that can occur during serial port operations.
#[derive(Debug, Error)]
pub enum AsciiSerialError {
    /// The port was not opened within `open_timeout`.
    #[error("open timeout")]
    OpenTimeout,
    /// A write or the wait for a reply exceeded `io_timeout`.
    #[error("I/O timeout")]
    IoTimeout,
    /// Underlying port error (missing device, unplugged cable, invalid UTF-8 on the wire, ...).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The port closed before a complete line was received.
    #[error("connection closed before response")]
    Eof,
    /// The OS refused to open the port because another process holds it (the port path carried).
    #[error("port is already in use: {0}")]
    PortBusy(String),
    /// A [`HardwareProtocol`] had a `type` other than `"ascii-serial"`.
    #[error("invalid protocol type: {0}")]
    InvalidProtocolType(String),
    /// A JSON payload could not be deserialised.
    #[error("backend config parse error: {0}")]
    ConfigParse(#[from] serde_json::Error),
    /// A `read_response` template is malformed or has nothing to capture.
    #[error("invalid response template: {0}")]
    TemplateSyntax(String),
    /// A received line does not fit the response template.
    #[error("response '{response}' does not match template '{template}'")]
    ResponseMismatch { template: String, response: String },
    /// A reply fit its template but holds no value usable for the channel's `response_mode`.
    #[error("invalid value: {0}")]
    InvalidValue(String),
    /// The device answered a write with an `ERROR` line.
    #[error("device error: {0}")]
    DeviceError(String),
}

impl AsciiSerialError {
    /// Returns `true` if the error is likely transient and safe to retry
    /// (timeouts, I/O errors, EOF and a busy port); configuration errors are not.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::OpenTimeout | Self::IoTimeout | Self::Io(_) | Self::Eof | Self::PortBusy(_)
        )
    }
}

/// Wire-format protocol block parsed from JSON config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HardwareProtocol {
    /// Protocol discriminator; must be `"ascii-serial"` (serialised as `type`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Port name, e.g. `COM3` or `/dev/ttyUSB0`.
    pub port_path: String,
    /// Baud rate.
    pub baud_rate: u32,
    /// Data bits per frame.
    pub data_bits: DataBits,
    /// Parity setting.
    pub parity: Parity,
    /// Stop bits per frame.
    pub stop_bits: StopBits,
    /// Line terminator used to frame requests and responses.
    pub line_ending: LineEnding,
    /// Open timeout in milliseconds; defaults to 2000 when `None`.
    pub open_timeout_ms: Option<u64>,
    /// I/O timeout in milliseconds; defaults to 2000 when `None`.
    pub io_timeout_ms: Option<u64>,
}

/// A single ASCII serial command with its target port.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Command {
    /// Optional caller-chosen identifier, echoed back in the matching [`CommandResult`].
    pub id: Option<String>,
    /// The request line, without line ending. An empty string sends nothing and only listens.
    pub request: String,
    /// Port the request is sent to.
    pub protocol: HardwareProtocol,
}

/// The response received for a single [`Command`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandResult {
    /// The `id` of the originating [`Command`].
    pub id: Option<String>,
    /// The accepted response line, without line ending.
    pub response: String,
}

/// A set of commands to execute sequentially, parsed from `{"commands": [...]}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchCommands {
    /// Commands, executed in order; the first failure aborts the batch.
    pub commands: Vec<Command>,
}

/// Responses from a [`BatchCommands`] execution.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchResult {
    /// One response per command, in command order.
    pub responses: Vec<CommandResult>,
}

/// Either a single [`Command`] or a [`BatchCommands`]; both forms deserialise from the same JSON.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Payload {
    Single(Command),
    Batch(BatchCommands),
}

/// Result of executing a [`Payload`]: a single response or a batch of responses.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandResultPayload {
    Single(CommandResult),
    Batch(BatchResult),
}

impl SerialConfig {
    /// Whether `other` opens the same port with the same line settings, so an open port can be reused.
    pub(crate) fn same_port_settings(&self, other: &Self) -> bool {
        self.port_path == other.port_path
            && self.baud_rate == other.baud_rate
            && self.data_bits == other.data_bits
            && self.parity == other.parity
            && self.stop_bits == other.stop_bits
    }

    /// Builds a `SerialConfig` from a `HardwareProtocol`, returning an error if `type` is not `"ascii-serial"`.
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

/// Parses a JSON string into a [`Command`].
pub fn parse_backend_command(json: &str) -> Result<Command, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

/// Parses a JSON string into a [`BatchCommands`].
pub fn parse_backend_batch(json: &str) -> Result<BatchCommands, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

/// Parses a JSON string into a [`Payload`] (single command or batch).
pub fn parse_backend_payload(json: &str) -> Result<Payload, AsciiSerialError> {
    Ok(serde_json::from_str(json)?)
}

/// Resolves a command's hardware protocol to a [`SerialConfig`]
/// (rejecting any type other than `ascii-serial`) and gets-or-creates the `Endpoint` for its port.
#[derive(Default)]
pub(crate) struct SerialBackend {
    endpoints: std::sync::Mutex<HashMap<String, Arc<Endpoint>>>,
}

impl SerialBackend {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the shared endpoint for the config's port path, creating it on first use.
    fn endpoint(&self, cfg: &SerialConfig) -> Arc<Endpoint> {
        let mut endpoints = self
            .endpoints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        endpoints.entry(cfg.port_path.clone()).or_default().clone()
    }

    /// Sends `cmd` on its port's shared connection and returns the first line `accept` approves.
    pub(crate) async fn execute_command_matching<F>(
        &self,
        cmd: &Command,
        accept: F,
    ) -> Result<CommandResult, AsciiSerialError>
    where
        F: Fn(&str) -> bool,
    {
        let cfg = SerialConfig::from_protocol(&cmd.protocol)?;
        let response = self
            .endpoint(&cfg)
            .exchange(&cfg, &cmd.request, accept)
            .await?;
        Ok(CommandResult {
            id: cmd.id.clone(),
            response,
        })
    }

    /// Sends `cmd` on its port's shared connection without waiting for a reply.
    pub(crate) async fn send_command(&self, cmd: &Command) -> Result<(), AsciiSerialError> {
        let cfg = SerialConfig::from_protocol(&cmd.protocol)?;
        self.endpoint(&cfg).send(&cfg, &cmd.request).await
    }
}

/// Sends a single request without waiting for a response.
///
/// Use this for commands the device does not acknowledge; [`exchange_line`]
/// would block until `io_timeout` elapses and then fail with
/// [`AsciiSerialError::IoTimeout`].
pub async fn send_line(cfg: &SerialConfig, request: &str) -> Result<(), AsciiSerialError> {
    let stream = open_stream_timed(cfg).await?;
    let (_reader, mut writer) = tokio::io::split(stream);
    write_request(&mut writer, cfg, request).await?;
    tracing::info!(port = %cfg.port_path, request, "ascii-serial TX (no response expected)");
    Ok(())
}

/// Opens the port, executes a single request and returns the response line, without the line ending.
pub async fn exchange_line(cfg: &SerialConfig, request: &str) -> Result<String, AsciiSerialError> {
    exchange_line_matching(cfg, request, |_| true).await
}

/// Like [`exchange_line`], but skips lines `accept` rejects until one matches or `io_timeout` elapses.
///
/// Some devices emit unsolicited lines (greeting banners, heartbeats, warnings)
/// that would otherwise be mistaken for the reply.
pub async fn exchange_line_matching<F>(
    cfg: &SerialConfig,
    request: &str,
    accept: F,
) -> Result<String, AsciiSerialError>
where
    F: Fn(&str) -> bool,
{
    let stream = open_stream_timed(cfg).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    exchange_on(&mut reader, &mut writer, cfg, request, accept).await
}

/// Writes `request` plus the line ending (nothing at all for an empty request), then reads lines
/// until `accept` approves one. Shared by the one-shot helpers and the persistent endpoint.
pub(crate) async fn exchange_on<R, W, F>(
    reader: &mut R,
    writer: &mut W,
    cfg: &SerialConfig,
    request: &str,
    accept: F,
) -> Result<String, AsciiSerialError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: Fn(&str) -> bool,
{
    if !request.is_empty() {
        write_request(writer, cfg, request).await?;
        tracing::info!(port = %cfg.port_path, request, "ascii-serial TX");
    }

    tokio::time::timeout(cfg.io_timeout, async {
        loop {
            let line = read_ascii_line(reader, cfg.line_ending)
                .await?
                .ok_or(AsciiSerialError::Eof)?;
            let accepted = accept(&line);
            tracing::info!(port = %cfg.port_path, line, accepted, "ascii-serial RX");
            if accepted {
                return Ok(line);
            }
        }
    })
    .await
    .map_err(|_| AsciiSerialError::IoTimeout)?
}

/// Writes `request` followed by the configured line ending, bounded by `io_timeout`.
pub(crate) async fn write_request<W>(
    writer: &mut W,
    cfg: &SerialConfig,
    request: &str,
) -> Result<(), AsciiSerialError>
where
    W: AsyncWrite + Unpin,
{
    let outbound = format!("{}{}", request, cfg.line_ending.as_str());
    tokio::time::timeout(cfg.io_timeout, async {
        writer.write_all(outbound.as_bytes()).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| AsciiSerialError::IoTimeout)??;
    Ok(())
}

/// Reads a single line from the reader, using the configured line ending as the terminator.
///
/// Returns `Ok(None)` on a clean EOF at a line boundary, [`AsciiSerialError::Eof`] on EOF in the
/// middle of a line, and an `InvalidData` I/O error if the line is not valid UTF-8.
pub(crate) async fn read_ascii_line<R>(
    reader: &mut R,
    line_ending: LineEnding,
) -> Result<Option<String>, AsciiSerialError>
where
    R: AsyncRead + Unpin,
{
    let terminator = line_ending.as_str().as_bytes();
    let mut bytes = Vec::new();

    loop {
        let byte = match reader.read_u8().await {
            Ok(byte) => byte,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && bytes.is_empty() => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(AsciiSerialError::Eof);
            }
            Err(error) => return Err(error.into()),
        };
        bytes.push(byte);

        if bytes.ends_with(terminator) {
            bytes.truncate(bytes.len() - terminator.len());
            let line = String::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            return Ok(Some(line));
        }
    }
}

/// Opens the port, failing with [`AsciiSerialError::OpenTimeout`] after `open_timeout`.
pub(crate) async fn open_stream_timed(
    cfg: &SerialConfig,
) -> Result<tokio_serial::SerialStream, AsciiSerialError> {
    tokio::time::timeout(cfg.open_timeout, open_stream(cfg))
        .await
        .map_err(|_| AsciiSerialError::OpenTimeout)?
}

async fn open_stream(cfg: &SerialConfig) -> Result<tokio_serial::SerialStream, AsciiSerialError> {
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

    tokio_serial::new(&cfg.port_path, cfg.baud_rate)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .open_native_async()
        .map_err(|error| {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                AsciiSerialError::PortBusy(cfg.port_path.clone())
            } else {
                AsciiSerialError::Io(error)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(line_ending: LineEnding) -> SerialConfig {
        SerialConfig {
            line_ending,
            io_timeout: Duration::from_millis(100),
            ..SerialConfig::default()
        }
    }

    #[tokio::test]
    async fn read_ascii_line_honours_each_line_ending() {
        for (line_ending, wire) in [
            (LineEnding::Lf, "A\nB\n"),
            (LineEnding::CrLf, "A\r\nB\r\n"),
            (LineEnding::Cr, "A\rB\r"),
        ] {
            let mut reader = wire.as_bytes();
            assert_eq!(read_ascii_line(&mut reader, line_ending).await.unwrap().as_deref(), Some("A"));
            assert_eq!(read_ascii_line(&mut reader, line_ending).await.unwrap().as_deref(), Some("B"));
            assert!(read_ascii_line(&mut reader, line_ending).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn read_ascii_line_keeps_a_bare_cr_inside_an_lf_line() {
        let mut reader = "A\rB\n".as_bytes();
        let line = read_ascii_line(&mut reader, LineEnding::Lf).await.unwrap();
        assert_eq!(line.as_deref(), Some("A\rB"));
    }

    #[tokio::test]
    async fn read_ascii_line_reports_eof_in_the_middle_of_a_line() {
        let mut reader = "partial".as_bytes();
        let error = read_ascii_line(&mut reader, LineEnding::Lf).await.unwrap_err();
        assert!(matches!(error, AsciiSerialError::Eof));
    }

    #[tokio::test]
    async fn read_ascii_line_rejects_invalid_utf8() {
        let mut reader: &[u8] = &[0xff, b'\n'];
        let error = read_ascii_line(&mut reader, LineEnding::Lf).await.unwrap_err();
        assert!(matches!(error, AsciiSerialError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData));
    }

    #[tokio::test]
    async fn exchange_on_writes_the_request_and_skips_rejected_lines() {
        let cfg = config(LineEnding::Cr);
        let (client, mut device) = tokio::io::duplex(64);
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);

        device.write_all(b"BANNER\rTEMP=22.5\r").await.unwrap();
        let line = exchange_on(&mut reader, &mut writer, &cfg, "TEMP?", |l| l.starts_with("TEMP"))
            .await
            .unwrap();
        assert_eq!(line, "TEMP=22.5");

        let mut sent = [0u8; 6];
        device.read_exact(&mut sent).await.unwrap();
        assert_eq!(&sent, b"TEMP?\r");
    }

    #[tokio::test]
    async fn exchange_on_with_an_empty_request_only_listens() {
        let cfg = config(LineEnding::Lf);
        let (client, mut device) = tokio::io::duplex(64);
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);

        device.write_all(b"ALIVE\n").await.unwrap();
        let line = exchange_on(&mut reader, &mut writer, &cfg, "", |_| true).await.unwrap();
        assert_eq!(line, "ALIVE");

        drop(writer);
        drop(reader);
        let mut rest = Vec::new();
        device.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "nothing must be written for an empty request");
    }

    #[tokio::test]
    async fn exchange_on_times_out_when_the_device_stays_silent() {
        let cfg = config(LineEnding::Lf);
        let (client, _device) = tokio::io::duplex(64);
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);

        let error = exchange_on(&mut reader, &mut writer, &cfg, "PING", |_| true)
            .await
            .unwrap_err();
        assert!(matches!(error, AsciiSerialError::IoTimeout));
    }

    #[tokio::test]
    async fn exchange_on_reports_eof_when_the_device_hangs_up() {
        let cfg = config(LineEnding::Lf);
        let (client, device) = tokio::io::duplex(64);
        drop(device);
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);

        let error = exchange_on(&mut reader, &mut writer, &cfg, "", |_| true)
            .await
            .unwrap_err();
        assert!(matches!(error, AsciiSerialError::Eof));
    }

    #[test]
    fn same_port_settings_ignores_timeouts_and_line_ending() {
        let a = SerialConfig::default();
        let b = SerialConfig {
            io_timeout: Duration::from_secs(9),
            line_ending: LineEnding::Cr,
            ..a.clone()
        };
        let c = SerialConfig { baud_rate: 115_200, ..a.clone() };
        assert!(a.same_port_settings(&b));
        assert!(!a.same_port_settings(&c));
    }
}
