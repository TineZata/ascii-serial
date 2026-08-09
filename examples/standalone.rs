use std::error::Error;
use std::time::Duration;

use ascii_serial::{exchange_line, DataBits, LineEnding, Parity, SerialConfig, StopBits};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: cargo run --example standalone -- <PORT_PATH> <REQUEST> [BAUD]");
        eprintln!("Example: cargo run --example standalone -- COM3 \"READ TEMP\" 9600");
        std::process::exit(2);
    }

    let port_path = args[1].clone();
    let request = args[2].clone();
    let baud_rate = args
        .get(3)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(9600);

    let cfg = SerialConfig {
        port_path,
        baud_rate,
        data_bits: DataBits::Eight,
        parity: Parity::None,
        stop_bits: StopBits::One,
        open_timeout: Duration::from_secs(3),
        io_timeout: Duration::from_secs(3),
        line_ending: LineEnding::Lf,
    };

    let response = exchange_line(&cfg, &request).await?;
    println!("request='{}' response='{}'", request, response);

    Ok(())
}
