use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Command, DataBits, HardwareProtocol, LineEnding, Parity, StopBits};

/// How a read response line is interpreted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AsciiResponseMode {
    /// Parses the response as a number, then applies `scale` and `offset`.
    Number,
    /// Parses the response as a boolean.
    Bool,
    /// Uses the response line as text. The default.
    #[default]
    Text,
    /// Treats receipt of any accepted response line as a pulse: the value is always `1`.
    ///
    /// Intended for heartbeat lines (e.g. `ALIVE`); set `read_response` to the literal to watch for
    /// so other lines are skipped. A stalled heartbeat surfaces as an I/O timeout.
    Presence,
}

/// Settings for one serial channel: port, read/write commands and value conversion.
///
/// Deserialises from a JSON block such as
/// `{ "port_path": "COM7", "baud_rate": 115200, "read_command": "STAT?", "read_response": "STAT %s" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Optional caller-chosen name, echoed in the command sent for this channel.
    #[serde(default)]
    pub id: Option<String>,
    /// Port name, e.g. `COM7` or `/dev/ttyUSB0`.
    pub port_path: String,
    /// Baud rate.
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
    /// Data bits per frame.
    #[serde(default = "default_data_bits")]
    pub data_bits: DataBits,
    /// Parity setting.
    #[serde(default = "default_parity")]
    pub parity: Parity,
    /// Stop bits per frame.
    #[serde(default = "default_stop_bits")]
    pub stop_bits: StopBits,
    /// Line terminator framing requests and responses.
    #[serde(default)]
    pub line_ending: LineEnding,
    /// Poll request sent on each read cycle.
    ///
    /// Omit for write-only channels. An empty string sends nothing and only listens for lines
    /// matching `read_response` (e.g. unsolicited heartbeats).
    #[serde(default)]
    pub read_command: Option<String>,
    /// Optional printf-style template describing the read response layout, e.g.
    /// `"TEMP=%f DegC"`. Lines that do not fit are skipped while waiting for the reply; every
    /// non-suppressed conversion is captured, the first supplies the value.
    /// When absent, any line is accepted and the whole line is interpreted per `response_mode`.
    #[serde(default)]
    pub read_response: Option<String>,
    /// Optional command template used for writes; `{value}` is replaced with the value.
    /// When absent, the raw value is sent as-is.
    #[serde(default)]
    pub write_command: Option<String>,
    /// Whether the device echoes a write or answers `ERROR…`. Set `false` for fire-and-forget
    /// commands, otherwise the write waits for a reply that never arrives.
    #[serde(default = "default_true")]
    pub write_expects_response: bool,
    /// Timeout in milliseconds for opening the port.
    #[serde(default = "default_timeout_ms")]
    pub open_timeout_ms: u64,
    /// Timeout in milliseconds for a single write or reply.
    #[serde(default = "default_timeout_ms")]
    pub io_timeout_ms: u64,
    /// Scale factor applied to parsed numeric values.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Offset applied after scaling.
    #[serde(default)]
    pub offset: f64,
    /// Response parse strategy.
    #[serde(default)]
    pub response_mode: AsciiResponseMode,
}

fn default_baud_rate() -> u32 {
    9600
}

fn default_data_bits() -> DataBits {
    DataBits::Eight
}

fn default_parity() -> Parity {
    Parity::None
}

fn default_stop_bits() -> StopBits {
    StopBits::One
}

fn default_true() -> bool {
    true
}

fn default_timeout_ms() -> u64 {
    2_000
}

fn default_scale() -> f64 {
    1.0
}

impl ChannelConfig {
    /// Wire-format protocol block for this port.
    pub fn protocol(&self) -> HardwareProtocol {
        HardwareProtocol {
            kind: "ascii-serial".to_string(),
            port_path: self.port_path.clone(),
            baud_rate: self.baud_rate,
            data_bits: self.data_bits,
            parity: self.parity,
            stop_bits: self.stop_bits,
            line_ending: self.line_ending,
            open_timeout_ms: Some(self.open_timeout_ms),
            io_timeout_ms: Some(self.io_timeout_ms),
        }
    }

    /// Builds a [`Command`] sending `request` on this channel's port.
    pub fn command(&self, request: impl Into<String>) -> Command {
        Command {
            id: self.id.clone(),
            request: request.into(),
            protocol: self.protocol(),
        }
    }
}

/// Retry behaviour for [`Context`](super::Context).
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// How long to wait between retry attempts on transient errors.
    pub retry_interval: Duration,
    /// If `true`, [`Context::execute_payload`](super::Context::execute_payload) retries on transient errors indefinitely.
    pub retry_forever: bool,
}

impl ClientConfig {
    /// Loads `ClientConfig` from `ASCII_SERIAL_RETRY_MS` and `ASCII_SERIAL_RETRY_FOREVER` environment variables.
    pub fn from_env() -> Self {
        let retry_interval_ms = std::env::var("ASCII_SERIAL_RETRY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000);

        let retry_forever = std::env::var("ASCII_SERIAL_RETRY_FOREVER")
            .map(|v| !v.eq_ignore_ascii_case("false") && !v.eq_ignore_ascii_case("no"))
            .unwrap_or(true);

        Self {
            retry_interval: Duration::from_millis(retry_interval_ms),
            retry_forever,
        }
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            retry_interval: Duration::from_secs(2),
            retry_forever: true,
        }
    }
}
