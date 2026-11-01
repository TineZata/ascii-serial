//! Channel-level reads and writes driven by a [`ChannelConfig`].
//!
//! [`Context::read_channel`] and [`Context::write_channel`] apply the configured
//! `read_command`, `read_response`, `response_mode`, `scale`/`offset`, `write_command`
//! and `write_expects_response`, so applications only decide when to poll.

use super::{AsciiResponseMode, ChannelConfig, Context};
use crate::{AsciiSerialError, Capture, ResponseTemplate};

/// A value read from a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The selected response text: the first template capture, or the whole line without one.
    pub text: String,
    /// The interpreted value: scaled for `number`, `0`/`1` for `bool`, `1` for `presence`, `0` for `text`.
    pub value: f64,
    /// Every field the template captured, in order (empty without a template or captures).
    pub captures: Vec<Capture>,
}

/// A [`ChannelConfig`] with its `read_response` template compiled once, ready for polling.
#[derive(Debug, Clone)]
pub struct ChannelReader {
    config: ChannelConfig,
    template: Option<ResponseTemplate>,
}

impl ChannelReader {
    /// Compiles the channel's `read_response` template, if any.
    pub fn new(config: ChannelConfig) -> Result<Self, AsciiSerialError> {
        let template = config
            .read_response
            .as_deref()
            .map(ResponseTemplate::compile)
            .transpose()?;
        Ok(Self { config, template })
    }

    /// The channel this reader polls.
    pub fn config(&self) -> &ChannelConfig {
        &self.config
    }

    /// Whether `line` is this channel's reply: it fits the template, or any line without one.
    ///
    /// Channels sharing a port receive each other's replies, so give each a
    /// `read_response` that only its own reply fits.
    pub fn accepts(&self, line: &str) -> bool {
        self.template
            .as_ref()
            .map_or(true, |template| template.captures(line).is_ok())
    }

    /// Interprets a reply line according to the channel's template and `response_mode`.
    ///
    /// Fails with [`AsciiSerialError::ResponseMismatch`] if the line does not fit the template
    /// and [`AsciiSerialError::InvalidValue`] if it holds no value for the mode.
    pub fn parse(&self, line: &str) -> Result<Reading, AsciiSerialError> {
        let captures = match &self.template {
            Some(template) => template.captures(line)?,
            None => Vec::new(),
        };
        let text = captures
            .first()
            .map_or_else(|| line.to_string(), |capture| capture.text.clone());

        let value = match self.config.response_mode {
            AsciiResponseMode::Number => {
                let raw = match captures.first() {
                    Some(capture) => capture.as_f64(),
                    None => first_number(&text),
                }
                .ok_or_else(|| AsciiSerialError::InvalidValue(format!("no number in '{text}'")))?;
                raw * self.config.scale + self.config.offset
            }
            AsciiResponseMode::Bool => parse_bool(&text)?,
            AsciiResponseMode::Text => 0.0,
            AsciiResponseMode::Presence => 1.0,
        };
        Ok(Reading { text, value, captures })
    }
}

impl ChannelConfig {
    /// Builds the write request for `value`: numeric values are converted back to raw units
    /// (`(value - offset) / scale`) in `number` mode, then substituted into `write_command`.
    pub fn write_request(&self, value: &str) -> Result<String, AsciiSerialError> {
        let value = value.trim();
        let outbound = if self.response_mode == AsciiResponseMode::Number {
            let physical: f64 = value.parse().map_err(|_| {
                AsciiSerialError::InvalidValue(format!("'{value}' is not a number"))
            })?;
            ((physical - self.offset) / self.scale).to_string()
        } else {
            value.to_string()
        };
        Ok(match &self.write_command {
            Some(template) => template.replace("{value}", &outbound),
            None => outbound,
        })
    }
}

impl Context {
    /// Polls the channel once: sends `read_command` (nothing when it is empty or absent,
    /// which only listens) and interprets the first reply the reader accepts.
    pub async fn read_channel(&self, reader: &ChannelReader) -> Result<Reading, AsciiSerialError> {
        let config = reader.config();
        let request = config.read_command.clone().unwrap_or_default();
        let reply = self
            .execute_command_matching(&config.command(request), |line| reader.accepts(line))
            .await?;
        reader.parse(&reply.response)
    }

    /// Writes `value` to the channel.
    ///
    /// With `write_expects_response` the device must echo the request or reply with a line
    /// starting `ERROR`, which fails with [`AsciiSerialError::DeviceError`]; otherwise the
    /// request is sent without waiting.
    pub async fn write_channel(
        &self,
        channel: &ChannelConfig,
        value: &str,
    ) -> Result<(), AsciiSerialError> {
        let request = channel.write_request(value)?;
        if !channel.write_expects_response {
            return self.send_command(&channel.command(request)).await;
        }
        let reply = self
            .execute_command_matching(&channel.command(request.clone()), |line| {
                line == request || line.starts_with("ERROR")
            })
            .await?;
        if reply.response.starts_with("ERROR") {
            return Err(AsciiSerialError::DeviceError(reply.response));
        }
        Ok(())
    }
}

/// The first number in `text`, splitting on whitespace and `,` `;` `=`.
fn first_number(text: &str) -> Option<f64> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '='))
        .find_map(|token| token.parse().ok())
}

fn parse_bool(text: &str) -> Result<f64, AsciiSerialError> {
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "open" => Ok(1.0),
        "0" | "false" | "off" | "closed" => Ok(0.0),
        _ => Err(AsciiSerialError::InvalidValue(format!("'{text}' is not a boolean"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(json: serde_json::Value) -> ChannelReader {
        let mut base = serde_json::json!({ "port_path": "COM7" });
        base.as_object_mut().unwrap().extend(json.as_object().unwrap().clone());
        ChannelReader::new(serde_json::from_value(base).unwrap()).unwrap()
    }

    #[test]
    fn number_mode_scales_the_template_capture() {
        let r = reader(serde_json::json!({
            "read_response": "TEMP=%f DegC", "response_mode": "number", "scale": 2.0, "offset": 1.0
        }));
        assert!(r.accepts("TEMP=21.5 DegC"));
        assert!(!r.accepts("PRESSURE=3"));
        let reading = r.parse("TEMP=21.5 DegC").unwrap();
        assert_eq!(reading.text, "21.5");
        assert_eq!(reading.value, 44.0);
    }

    #[test]
    fn number_mode_reads_hex_captures_and_untemplated_lines() {
        let hex = reader(serde_json::json!({ "read_response": "REG=%x", "response_mode": "number" }));
        assert_eq!(hex.parse("REG=0x1F").unwrap().value, 31.0);

        let plain = reader(serde_json::json!({ "response_mode": "number" }));
        assert!(plain.accepts("anything"));
        assert_eq!(plain.parse("VALUE = 12.5 mm").unwrap().value, 12.5);
        assert!(matches!(plain.parse("OFFLINE"), Err(AsciiSerialError::InvalidValue(_))));
    }

    #[test]
    fn bool_text_and_presence_modes() {
        let flag = reader(serde_json::json!({ "read_response": "EN=%s", "response_mode": "bool" }));
        assert_eq!(flag.parse("EN=ON").unwrap().value, 1.0);
        assert_eq!(flag.parse("EN=closed").unwrap().value, 0.0);
        assert!(matches!(flag.parse("EN=maybe"), Err(AsciiSerialError::InvalidValue(_))));

        let text = reader(serde_json::json!({ "read_response": "MSG=%s", "response_mode": "text" }));
        let reading = text.parse("MSG=hello world").unwrap();
        assert_eq!((reading.text.as_str(), reading.value), ("hello world", 0.0));

        let alive = reader(serde_json::json!({ "read_response": "ALIVE", "response_mode": "presence" }));
        assert!(alive.accepts("ALIVE") && !alive.accepts("DEAD"));
        assert_eq!(alive.parse("ALIVE").unwrap().value, 1.0);
    }

    #[test]
    fn every_capture_is_returned() {
        let r = reader(serde_json::json!({
            "read_response": "STAT Baud = %d, Parity = %s, Stop Bits = %*d, Data Bits = %s"
        }));
        let reading = r
            .parse("STAT Baud = 115200, Parity = None, Stop Bits = 1, Data Bits = 8")
            .unwrap();
        let texts: Vec<_> = reading.captures.iter().map(Capture::as_str).collect();
        assert_eq!(texts, ["115200", "None", "8"]);
        assert_eq!(reading.text, "115200");
    }

    #[test]
    fn mismatching_lines_are_rejected() {
        let r = reader(serde_json::json!({ "read_response": "TEMP=%f" }));
        assert!(matches!(r.parse("PRESSURE=3"), Err(AsciiSerialError::ResponseMismatch { .. })));
    }

    #[test]
    fn write_request_converts_to_raw_units_and_fills_the_template() {
        let cfg: ChannelConfig = serde_json::from_value(serde_json::json!({
            "port_path": "COM7", "write_command": "SET {value}",
            "response_mode": "number", "scale": 2.0, "offset": 1.0
        }))
        .unwrap();
        assert_eq!(cfg.write_request("11").unwrap(), "SET 5");
        assert!(matches!(cfg.write_request("x"), Err(AsciiSerialError::InvalidValue(_))));
    }

    #[test]
    fn channel_command_targets_its_port() {
        let cfg: ChannelConfig = serde_json::from_value(serde_json::json!({
            "id": "t", "port_path": "COM7", "baud_rate": 115200, "line_ending": "crlf"
        }))
        .unwrap();
        let cmd = cfg.command("STAT?");
        assert_eq!(cmd.id.as_deref(), Some("t"));
        assert_eq!((cmd.protocol.port_path.as_str(), cmd.protocol.baud_rate), ("COM7", 115200));
    }
}
