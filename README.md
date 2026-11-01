# ascii-serial

Tokio-based ASCII request/response client over serial ports.

## Typical use

Most applications poll a device that answers ascii text commands (`STAT?` → `STAT Baud = 115200, ...`):

1. Describe each device value as a [`ChannelConfig`](#channelconfig) (usually a block in the app's JSON config).
2. Share one [`Context`](#context) across the app; it keeps one connection per port.
3. **Read** with `Context::read_channel` and a `ChannelReader`: it sends `read_command`, skips lines
   that are not the reply, and parses the reply with the `read_response` [template](#response-templates).
4. **Write** with `Context::write_channel`, which fills `write_command` and waits for the device's
   acknowledgement when `write_expects_response` is set.

| Most used | Purpose |
|-----------|---------|
| `ChannelConfig` | Deserialisable per-channel settings; `command(request)` builds what the client sends |
| `ChannelReader` | A `ChannelConfig` with its template compiled once, for polling |
| `Context` | Shared client: `read_channel`, `write_channel`, `execute_command_matching`, `send_command`, `on_connected` / `on_disconnected` |
| `ResponseTemplate` | printf-style value extraction from reply lines (`"TEMP=%f DegC"`) |
| *Less common* | `Payload` / `execute_json` for JSON batches; `exchange_line` / `send_line` one-shot helpers |

LF, CRLF and CR line framing are supported, for requests and replies alike.

## Quick start

```rust,no_run
use ascii_serial::{ChannelConfig, ChannelReader, Context};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
// 1. Describe the channel.
let channel: ChannelConfig = serde_json::from_str(r#"{
    "port_path": "COM7", "baud_rate": 115200, "line_ending": "crlf",
    "read_command": "STAT?",
    "read_response": "STAT Baud = %d, Parity = %s, Stop Bits = %*d, Data Bits = %s"
}"#)?;

// 2. One shared context for the whole app.
let context = Context::from_env();
context.on_connected(|port| println!("connected: {port}"));
context.on_disconnected(|port, reason| println!("disconnected: {port} ({reason})"));

// 3. Read: send the poll and interpret the reply.
let reader = ChannelReader::new(channel)?;
let reading = context.read_channel(&reader).await?;
// reading.captures = ["115200", "None", "8"]; reading.text = "115200" (the first capture)
# let _ = reading;
# Ok(())
# }
```

## `ChannelConfig`

Deserialises from JSON; only `port_path` is required. `command(request)` builds the `Command`
the client methods take, and `protocol()` the wire-format `HardwareProtocol`.

| Field | Default | Meaning |
|-------|---------|---------|
| `id` | none | Optional name, echoed in the command sent for this channel |
| `port_path` | required | `COM7`, `/dev/ttyUSB0`, ... |
| `baud_rate` / `data_bits` / `parity` / `stop_bits` | `9600` / `eight` / `none` / `one` | Port framing (`five`..`eight`, `none`/`odd`/`even`, `one`/`two`) |
| `line_ending` | `lf` | `lf`, `crlf` or `cr` |
| `read_command` | none | Poll request; omit for write-only channels, `""` to only listen |
| `read_response` | none | [Template](#response-templates) a reply must fit; without one any line is accepted |
| `write_command` | none | Template where `{value}` is replaced; otherwise the raw value is sent |
| `write_expects_response` | `true` | Set `false` for fire-and-forget writes |
| `open_timeout_ms` / `io_timeout_ms` | `2000` | Timeouts for opening the port and for one write or reply |
| `scale` / `offset` | `1.0` / `0.0` | Applied to numeric values |
| `response_mode` | `text` | `number`, `bool`, `text`, or `presence` (any matching line is a pulse, value `1`) |

[examples/config.json](examples/config.json) is a full `{"channels": [...]}` document;
`cargo run --example backend -- examples/config.json` polls it every two seconds and logs
each reading (set `RUST_LOG` to change the default `info` level).

## Response templates

`read_response` is a printf-style pattern for the **whole** reply line. Literal text must match
exactly; conversions pick out the fields:

| Specifier | Matches |
|-----------|---------|
| `%d` `%i` `%u` | signed decimal integer |
| `%f` `%e` `%g` | decimal number, optional fraction and exponent |
| `%x` `%X` | hexadecimal integer, optional `0x` |
| `%s` | free text up to the next literal (or to the end of the line) |
| `%%` | a literal `%` |

- `%*d`, `%*s`, ... match a field but **discard** it, as in `scanf`.
- Matching is anchored: after trimming the line, all of it must be consumed. Numeric
  conversions skip leading spaces and tabs.
- Adjacent conversions need literal text between them (`%d%d` is rejected).
- Every non-suppressed conversion is captured, in order. `Reading.text` is the first one and
  `Reading.captures` has all of them (`as_str`, `as_i64`, `as_f64`).

```text
reply:     STAT Baud = 115200, Parity = None, Stop Bits = 1, Data Bits = 8

"STAT Baud = %d, Parity = %s, Stop Bits = %d, Data Bits = %s"    -> 115200, None, 1, 8
"STAT Baud = %d, Parity = %*s, Stop Bits = %*d, Data Bits = %*s" -> 115200
"STAT Baud = %*d, Parity = %s, Stop Bits = %*d, Data Bits = %*s" -> None
```

Templates also decide *which line is the reply*: `read_channel` skips lines that do not fit
(banners, heartbeats, other channels' replies) until one does or `io_timeout_ms` elapses.
Channels on one port share its connection, so give each a template only its own reply fits.
A wrong template shows up as an I/O timeout, because the real reply is skipped; check the
`ascii-serial RX ... accepted=false` log line to see what the device actually sent, such as
unexpected commas or spacing.

`ResponseTemplate`, `parse_response_template` and `parse_first_value` (in `ascii_serial::template`)
use templates directly:

```rust
use ascii_serial::template::parse_response_template;

let captures = parse_response_template("TEMP=%f DegC", "TEMP=22.5 DegC").unwrap();
assert_eq!(captures[0].as_f64(), Some(22.5));
```

### Reads and writes

- `Reading { text, value, captures }`: `value` is the first capture scaled (`* scale + offset`)
  in `number` mode, `0`/`1` in `bool` mode (`1 true on open` / `0 false off closed`), `1` for
  `presence` and `0` for `text`.
- `Context::read_channel(&reader)` sends `read_command` (an empty or absent one only listens)
  and parses the first accepted reply.
- `Context::write_channel(&config, value)` converts a `number`-mode value back to raw units
  (`(value - offset) / scale`) and substitutes it into `write_command`. With
  `write_expects_response` the device must echo the request or reply with a line starting
  `ERROR` (`AsciiSerialError::DeviceError`); otherwise nothing is awaited.

## `Context`

Each port path gets one persistent connection, opened on first use and shared by every request
(and every clone of the `Context`). Requests to the same port are queued, not rejected. The
connection is reopened after a failure or a reply timeout. Create it with `Context::from_env()`
or `Context::new(ClientConfig { .. })`.

| Method | Behaviour |
|--------|-----------|
| `read_channel(&reader)` / `write_channel(&config, value)` | Channel-level poll and write |
| `execute_command_matching(&cmd, accept)` | Sends one command and returns the first reply line `accept` approves; no retry |
| `send_command(&cmd)` | Sends without waiting for a reply |
| `execute_payload(&payload)` / `execute_payload_once(&payload)` | Runs a JSON `Command` or `{"commands": [...]}` batch (see `parse_backend_payload`), with or without retrying per `ClientConfig` |
| `on_connected` / `on_disconnected` | Per-port callbacks |

`on_connected` fires when a port goes from unknown/disconnected to connected. `on_disconnected`
fires on every connection-level failure (open failure, I/O error, EOF) with the error text.
A reply timeout is only logged: the device may simply ignore that command.

`Context::from_env` / `ClientConfig::from_env` read `ASCII_SERIAL_RETRY_MS` (retry interval,
default `2000`) and `ASCII_SERIAL_RETRY_FOREVER` (default `true`; `false` or `no` stops
`execute_payload` retrying after the first failure). The channel methods never retry; the
caller decides when to poll again.

## Errors

`AsciiSerialError` separates transient failures (`OpenTimeout`, `IoTimeout`, `Io`, `Eof`,
`PortBusy`; see `is_retryable`) from configuration errors (`InvalidProtocolType`,
`ConfigParse`, `TemplateSyntax`), which are never retried. `ResponseMismatch`, `InvalidValue`
and `DeviceError` report a reply that arrived but is unusable. Open failures keep their
`std::io::ErrorKind`, and `PortBusy` means the OS denied access to a port another process
holds.
