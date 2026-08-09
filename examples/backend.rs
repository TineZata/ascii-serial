use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ascii_serial::{
    client::{ClientConfig, Context},
    parse_backend_payload, Command, Payload,
    CommandResultPayload, AsciiSerialError,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

fn resolve_config_path(arg_path: &str) -> Result<PathBuf, std::io::Error> {
    let direct = PathBuf::from(arg_path);
    if direct.exists() {
        return Ok(direct);
    }

    if !Path::new(arg_path).is_absolute() {
        let from_examples = Path::new("examples").join(arg_path);
        if from_examples.exists() {
            return Ok(from_examples);
        }

        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "config file not found; tried '{}' and '{}'",
                direct.display(),
                from_examples.display()
            ),
        ));
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("config file not found: '{}'", direct.display()),
    ))
}

fn print_payload_result(result: CommandResultPayload) {
    match result {
        CommandResultPayload::Single(single) => {
            if let Some(id) = single.id {
                info!(id = %id, response = %single.response, "serial response");
            } else {
                info!(response = %single.response, "serial response");
            }
        }
        CommandResultPayload::Batch(batch) => {
            for entry in batch.responses {
                if let Some(id) = entry.id {
                    info!(id = %id, response = %entry.response, "serial response");
                } else {
                    info!(response = %entry.response, "serial response");
                }
            }
        }
    }
}

fn payload_ports(payload: &Payload) -> Vec<String> {
    match payload {
        Payload::Single(cmd) => vec![cmd.protocol.port_path.clone()],
        Payload::Batch(batch) => {
            let mut ports = Vec::with_capacity(batch.commands.len());
            for Command { protocol, .. } in &batch.commands {
                if !ports.iter().any(|p| p == &protocol.port_path) {
                    ports.push(protocol.port_path.clone());
                }
            }
            ports
        }
    }
}

fn print_available_ports() {
    match tokio_serial::available_ports() {
        Ok(ports) if ports.is_empty() => {
            warn!("No serial ports detected on this system");
        }
        Ok(ports) => {
            info!("Detected serial ports");
            for p in ports {
                info!(port = %p.port_name, "serial port");
            }
        }
        Err(e) => {
            warn!(error = %e, "Could not query available serial ports");
        }
    }
}

fn diagnose_error(err: &AsciiSerialError, payload: &Payload) {
    if let AsciiSerialError::Io(ioe) = err {
        if ioe.kind() == std::io::ErrorKind::Other {
            let msg = ioe.to_string().to_lowercase();
            if msg.contains("cannot find the file") {
                let ports = payload_ports(payload);
                warn!(ports = %ports.join(", "), "Configured serial port(s) not found");
                print_available_ports();
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let config_arg = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "examples/config.json".to_string());

    let config_path = resolve_config_path(&config_arg)?;
    info!(path = %config_path.display(), "loading backend config");
    let raw = std::fs::read_to_string(&config_path)?;
    let payload = parse_backend_payload(&raw)?;
    let configured_ports = payload_ports(&payload);

    let context = Context::new(ClientConfig {
        retry_interval: Duration::from_secs(2),
        retry_forever: false,
    });
    context.on_connected(|port| {
        info!(port = %port, "connected callback");
    });
    context.on_disconnected(|port, reason| {
        warn!(port = %port, reason = %reason, "disconnected callback");
    });

    let retry_interval = Duration::from_secs(2);
    info!(retry_ms = retry_interval.as_millis(), "starting backend poll loop");
    loop {
        match context.execute_payload_once(&payload).await {
            Ok(result) => {
                print_payload_result(result);
            }
            Err(err) => {
                for port in &configured_ports {
                    error!(port = %port, error = %err, "connection failed");
                }
                diagnose_error(&err, &payload);
            }
        }

        tokio::time::sleep(retry_interval).await;
    }
}
