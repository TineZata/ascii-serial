//! `ClientConfig::from_env`. Kept in its own test binary with a single test because it mutates
//! process-wide environment variables.

use std::time::Duration;

use ascii_serial::ClientConfig;

#[test]
fn from_env_reads_the_retry_variables() {
    std::env::remove_var("ASCII_SERIAL_RETRY_MS");
    std::env::remove_var("ASCII_SERIAL_RETRY_FOREVER");
    let defaults = ClientConfig::from_env();
    assert_eq!(defaults.retry_interval, Duration::from_secs(2));
    assert!(defaults.retry_forever);

    std::env::set_var("ASCII_SERIAL_RETRY_MS", "150");
    std::env::set_var("ASCII_SERIAL_RETRY_FOREVER", "No");
    let config = ClientConfig::from_env();
    assert_eq!(config.retry_interval, Duration::from_millis(150));
    assert!(!config.retry_forever);

    std::env::set_var("ASCII_SERIAL_RETRY_MS", "soon");
    std::env::set_var("ASCII_SERIAL_RETRY_FOREVER", "false");
    let config = ClientConfig::from_env();
    assert_eq!(config.retry_interval, Duration::from_secs(2));
    assert!(!config.retry_forever);

    std::env::remove_var("ASCII_SERIAL_RETRY_MS");
    std::env::remove_var("ASCII_SERIAL_RETRY_FOREVER");
}
