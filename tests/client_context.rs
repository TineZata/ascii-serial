//! `Context` behaviour that needs no hardware: callbacks and error handling against a missing port.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ascii_serial::client::{ClientConfig, Context};
use ascii_serial::{parse_backend_payload, AsciiSerialError, Payload};

const MISSING_PORT: &str = "ascii-serial-test-no-such-port";

fn once() -> ClientConfig {
    ClientConfig { retry_interval: Duration::from_millis(5), retry_forever: false }
}

fn payload_json(port: &str, kind: &str) -> String {
    format!(
        r#"{{ "id": "t", "request": "PING", "protocol": {{ "type": "{kind}", "port_path": "{port}",
            "baud_rate": 9600, "data_bits": "eight", "parity": "none", "stop_bits": "one",
            "line_ending": "lf", "open_timeout_ms": 300, "io_timeout_ms": 300 }} }}"#
    )
}

#[tokio::test]
async fn every_failed_attempt_fires_the_disconnected_callback() {
    let context = Context::new(once());
    let connected = Arc::new(AtomicUsize::new(0));
    let reasons = Arc::new(Mutex::new(Vec::new()));
    {
        let connected = connected.clone();
        context.on_connected(move |_| {
            connected.fetch_add(1, Ordering::SeqCst);
        });
        let reasons = reasons.clone();
        context.on_disconnected(move |port, reason| reasons.lock().unwrap().push((port, reason)));
    }

    let payload = parse_backend_payload(&payload_json(MISSING_PORT, "ascii-serial")).unwrap();
    for _ in 0..2 {
        let error = context.execute_payload_once(&payload).await.unwrap_err();
        assert!(matches!(error, AsciiSerialError::Io(_) | AsciiSerialError::PortBusy(_)));
    }

    assert_eq!(connected.load(Ordering::SeqCst), 0);
    let reasons = reasons.lock().unwrap();
    assert_eq!(reasons.len(), 2, "each failed attempt must be reported");
    assert!(reasons.iter().all(|(port, _)| port == MISSING_PORT));
}

#[tokio::test]
async fn a_missing_port_keeps_its_error_kind() {
    let context = Context::new(once());
    let payload = parse_backend_payload(&payload_json(MISSING_PORT, "ascii-serial")).unwrap();

    match context.execute_payload_once(&payload).await.unwrap_err() {
        AsciiSerialError::Io(error) => assert_ne!(error.kind(), std::io::ErrorKind::Other),
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn an_invalid_protocol_type_is_not_retried_even_when_retrying_forever() {
    let context = Context::new(ClientConfig {
        retry_interval: Duration::from_millis(5),
        retry_forever: true,
    });
    let payload = parse_backend_payload(&payload_json(MISSING_PORT, "ascii-tcp")).unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), context.execute_payload(&payload))
        .await
        .expect("a non-retryable error must end the retry loop");
    assert!(matches!(result.unwrap_err(), AsciiSerialError::InvalidProtocolType(_)));
}

#[tokio::test]
async fn send_command_reports_a_missing_port() {
    let context = Context::new(once());
    let command = match parse_backend_payload(&payload_json(MISSING_PORT, "ascii-serial")).unwrap() {
        Payload::Single(command) => command,
        Payload::Batch(_) => unreachable!(),
    };
    assert!(context.send_command(&command).await.is_err());
}
