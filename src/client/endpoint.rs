//! One persistent serial connection per port path, shared by every request through a `Context`.
//!
//! The port stays open between requests, so devices that reset when opened (USB-serial boards)
//! are not reset on every poll and bytes buffered between requests are not lost. Requests to the
//! same port are serialised on the endpoint's lock.
//!
//! A link is dropped, and the port reopened by the next request, whenever its state is uncertain:
//! after any failure, a reply timeout (a late reply must not be mistaken for the next one) or a
//! cancelled request (which may have left a half-read line).

use tokio::io::{BufReader, ReadHalf, WriteHalf};
use tokio::sync::Mutex;
use tokio_serial::SerialStream;

use crate::{exchange_on, open_stream_timed, write_request, AsciiSerialError, SerialConfig};

struct Link {
    config: SerialConfig,
    reader: BufReader<ReadHalf<SerialStream>>,
    writer: WriteHalf<SerialStream>,
}

#[derive(Default)]
pub(crate) struct Endpoint {
    link: Mutex<Option<Link>>,
}

impl Endpoint {
    /// Sends `request` and returns the first received line `accept` approves.
    pub(crate) async fn exchange<F>(
        &self,
        config: &SerialConfig,
        request: &str,
        accept: F,
    ) -> Result<String, AsciiSerialError>
    where
        F: Fn(&str) -> bool,
    {
        let line = self.run(config, request, Some(&accept)).await?;
        Ok(line.unwrap_or_default())
    }

    /// Sends `request` without waiting for a reply.
    pub(crate) async fn send(
        &self,
        config: &SerialConfig,
        request: &str,
    ) -> Result<(), AsciiSerialError> {
        self.run(config, request, None).await.map(|_| ())
    }

    /// Runs one request on the open link: an exchange when `accept` is given, otherwise a plain
    /// write. The link is taken out of the lock while it runs, so a failed or cancelled request
    /// drops it, and is put back only on success. If a reused link fails with an I/O error or EOF
    /// (the device was unplugged or reset), the request is retried once on a freshly opened port.
    async fn run(
        &self,
        config: &SerialConfig,
        request: &str,
        accept: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Option<String>, AsciiSerialError> {
        let mut slot = self.link.lock().await;
        let reusable = slot
            .take()
            .filter(|link| link.config.same_port_settings(config));

        let (mut link, reused) = match reusable {
            Some(link) => (link, true),
            None => (open_link(config).await?, false),
        };

        match run_on(&mut link, config, request, accept).await {
            Ok(value) => {
                *slot = Some(link);
                Ok(value)
            }
            Err(AsciiSerialError::Io(_) | AsciiSerialError::Eof) if reused => {
                // Release the stale handle before reopening the same port.
                drop(link);
                let mut link = open_link(config).await?;
                let value = run_on(&mut link, config, request, accept).await?;
                *slot = Some(link);
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

async fn run_on(
    link: &mut Link,
    config: &SerialConfig,
    request: &str,
    accept: Option<&dyn Fn(&str) -> bool>,
) -> Result<Option<String>, AsciiSerialError> {
    match accept {
        Some(accept) => exchange_on(&mut link.reader, &mut link.writer, config, request, accept)
            .await
            .map(Some),
        None => {
            write_request(&mut link.writer, config, request).await?;
            tracing::info!(port = %config.port_path, request, "ascii-serial TX (no response expected)");
            Ok(None)
        }
    }
}

async fn open_link(config: &SerialConfig) -> Result<Link, AsciiSerialError> {
    let stream = open_stream_timed(config).await?;
    let (reader, writer) = tokio::io::split(stream);
    Ok(Link {
        config: config.clone(),
        reader: BufReader::new(reader),
        writer,
    })
}
