//! Payload parsing and protocol-to-config conversion.

use std::time::Duration;

use ascii_serial::{
    parse_backend_batch, parse_backend_command, parse_backend_payload, AsciiSerialError,
    CommandResultPayload, DataBits, LineEnding, Parity, Payload, SerialConfig, StopBits,
};

const PROTOCOL: &str = r#"{ "type": "ascii-serial", "port_path": "COM9", "baud_rate": 19200,
    "data_bits": "seven", "parity": "even", "stop_bits": "two", "line_ending": "crlf",
    "open_timeout_ms": 500 }"#;

fn command_json(id: &str) -> String {
    format!(r#"{{ "id": "{id}", "request": "TEMP?", "protocol": {PROTOCOL} }}"#)
}

#[test]
fn single_and_batch_payloads_parse_to_the_right_variant() {
    assert!(matches!(
        parse_backend_payload(&command_json("a")).unwrap(),
        Payload::Single(_)
    ));

    let batch = format!(r#"{{ "commands": [{}, {}] }}"#, command_json("a"), command_json("b"));
    match parse_backend_payload(&batch).unwrap() {
        Payload::Batch(batch) => assert_eq!(batch.commands.len(), 2),
        Payload::Single(_) => panic!("expected a batch"),
    }
    assert_eq!(parse_backend_batch(&batch).unwrap().commands.len(), 2);
    assert_eq!(parse_backend_command(&command_json("a")).unwrap().id.as_deref(), Some("a"));
}

#[test]
fn malformed_json_is_a_config_parse_error() {
    let error = parse_backend_payload("{ nope").unwrap_err();
    assert!(matches!(error, AsciiSerialError::ConfigParse(_)));
    assert!(!error.is_retryable());
}

#[test]
fn protocol_converts_to_serial_config_with_default_timeouts() {
    let command = parse_backend_command(&command_json("a")).unwrap();
    let config = SerialConfig::from_protocol(&command.protocol).unwrap();

    assert_eq!(config.port_path, "COM9");
    assert_eq!(config.baud_rate, 19_200);
    assert_eq!(config.data_bits, DataBits::Seven);
    assert_eq!(config.parity, Parity::Even);
    assert_eq!(config.stop_bits, StopBits::Two);
    assert!(matches!(config.line_ending, LineEnding::CrLf));
    assert_eq!(config.open_timeout, Duration::from_millis(500));
    assert_eq!(config.io_timeout, Duration::from_millis(2_000));
}

#[test]
fn foreign_protocol_types_are_rejected() {
    let json = command_json("a").replace("ascii-serial", "ascii-tcp");
    let command = parse_backend_command(&json).unwrap();
    let error = SerialConfig::from_protocol(&command.protocol).unwrap_err();
    assert!(matches!(error, AsciiSerialError::InvalidProtocolType(kind) if kind == "ascii-tcp"));
}

#[test]
fn retryable_errors_are_the_transient_ones() {
    assert!(AsciiSerialError::OpenTimeout.is_retryable());
    assert!(AsciiSerialError::IoTimeout.is_retryable());
    assert!(AsciiSerialError::Eof.is_retryable());
    assert!(AsciiSerialError::PortBusy("COM1".into()).is_retryable());
    assert!(!AsciiSerialError::InvalidProtocolType("x".into()).is_retryable());
}

#[test]
fn result_payload_round_trips_through_json() {
    let json = r#"{ "responses": [ { "id": "a", "response": "OK" } ] }"#;
    let parsed: CommandResultPayload = serde_json::from_str(json).unwrap();
    assert!(matches!(parsed, CommandResultPayload::Batch(batch) if batch.responses.len() == 1));
}
