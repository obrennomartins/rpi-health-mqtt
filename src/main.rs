//! Command-line entry point for the Raspberry Pi health monitor.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();

    if matches!(arguments.next().as_deref(), Some(value) if value == "--version")
        && arguments.next().is_none()
    {
        println!("rpi-health-mqtt {}", rpi_health_mqtt::VERSION);
        return ExitCode::SUCCESS;
    }

    eprintln!("The service is not configured yet. Use --version to inspect the build.");
    ExitCode::FAILURE
}
