use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ascii_serial::{
    client::{ClientConfig, Context},
    AsciiSerialError, ChannelConfig, ChannelReader, Reading,
};
use serde::Deserialize;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Deserialize)]
struct BackendConfig {
    channels: Vec<ChannelConfig>,
}

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

fn print_reading(channel: &ChannelConfig, reading: &Reading) {
    let id = channel.id.as_deref().unwrap_or("-");
    let captures: Vec<&str> = reading.captures.iter().map(|c| c.as_str()).collect();
    info!(
        id = %id,
        text = %reading.text,
        value = reading.value,
        captures = ?captures,
        "serial reading"
    );
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

fn diagnose_error(err: &AsciiSerialError, channel: &ChannelConfig) {
    match err {
        AsciiSerialError::Io(ioe) if ioe.kind() == std::io::ErrorKind::NotFound => {
            warn!(port = %channel.port_path, "Configured serial port not found");
            print_available_ports();
        }
        AsciiSerialError::PortBusy(port) => {
            warn!(port = %port, "Serial port is held by another process");
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
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
    let config: BackendConfig = serde_json::from_str(&raw)?;
    let readers = config
        .channels
        .into_iter()
        .filter(|channel| channel.read_command.is_some())
        .map(ChannelReader::new)
        .collect::<Result<Vec<_>, _>>()?;

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
        for reader in &readers {
            let channel = reader.config();
            match context.read_channel(reader).await {
                Ok(reading) => print_reading(channel, &reading),
                Err(err) => {
                    let id = channel.id.as_deref().unwrap_or("-");
                    error!(id = %id, error = %err, "channel read failed");
                    diagnose_error(&err, channel);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
