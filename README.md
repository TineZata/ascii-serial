# ascii-serial

Tokio-based ASCII request/response client over serial ports.

## Architecture

- `client::Context` executes a single command or a batch of commands (a `Payload`),
  optionally retrying forever on transient port failures, and reports connect/disconnect
  transitions per port path via callbacks.
- `parse_backend_payload` / `parse_backend_command` / `parse_backend_batch` parse JSON
  into a `Payload` (`Single` or `Batch`), a single `Command`, or `BatchCommands`
  respectively. Every parsed value carries its own `HardwareProtocol` (port path, baud
  rate, framing, timeouts), so a batch can fan out to multiple different ports.
- LF, CRLF, and CR line framing are supported.
- Port access is serialised per path: concurrent requests to the same port are queued,
  not rejected.

## Usage

```rust,no_run
use ascii_serial::{client::Context, parse_backend_payload};

# async fn run() -> Result<(), ascii_serial::AsciiSerialError> {
let payload = parse_backend_payload(r#"{
    "id": "temperature",
    "request": "TEMP?",
    "protocol": {
        "type": "ascii-serial",
        "port_path": "COM3",
        "baud_rate": 9600,
        "data_bits": "eight",
        "parity": "none",
        "stop_bits": "one",
        "line_ending": "lf"
    }
}"#)?;

let context = Context::from_env();
context.on_connected(|port| println!("connected: {port}"));
context.on_disconnected(|port, reason| println!("disconnected: {port} ({reason})"));

// Runs once and returns whatever error occurred (no retrying).
let result = context.execute_payload_once(&payload).await?;

// Retries on transient errors (open/IO timeout, I/O error, EOF, port busy) according to
// `ClientConfig`; never returns on a retryable error unless `retry_forever` is false.
let result = context.execute_payload(&payload).await?;
# let _ = result;
# Ok(())
# }
```

`parse_backend_payload` also accepts a `{"commands": [...]}` batch document (see
[examples/config.json](examples/config.json)); `execute_payload`/`execute_payload_once`
run every command in the batch sequentially against its own port and return a
`CommandResultPayload::Batch`.

## Environment variables

`Context::from_env` / `ClientConfig::from_env` read:

- `ASCII_SERIAL_RETRY_MS` — retry interval in milliseconds. Defaults to `2000`.
- `ASCII_SERIAL_RETRY_FOREVER` — defaults to `true`; set to `false` or `no` (case
  insensitive) to make `execute_payload` behave like `execute_payload_once` and stop
  retrying after the first failure.

`on_connected` fires once per port the first time it transitions from
unknown/disconnected to connected; it does not re-fire on every successful call.
`on_disconnected` fires on every failed attempt, with the error's `Display` text as the
reason.

## Typed values are an example concern, not a library one

`Command.request` / `CommandResult.response` are plain `String`s in the library — the
crate models *how to reach a device* (`HardwareProtocol`, line ending, timeouts), not
*what shape its reply takes*.
