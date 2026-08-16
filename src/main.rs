//! Command-line entry point for the monitoring service.

use std::process::ExitCode;

use clap::Parser;
use rpi_health_mqtt::{
    cli::{Cli, Command, CONFIG_ERROR, DIAGNOSTIC_ERROR, RUNTIME_ERROR, SUCCESS},
    collector::Collector,
    config::{Config, MqttCredentials},
};

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => ExitCode::from(run(cli)),
        Err(error) => {
            let code = if error.use_stderr() {
                error.exit_code()
            } else {
                i32::from(SUCCESS)
            };
            let _ = error.print();
            ExitCode::from(u8::try_from(code).unwrap_or(2))
        }
    }
}

fn run(cli: Cli) -> u8 {
    let config = match Config::load(&cli.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Configuration error: {error}");
            return CONFIG_ERROR;
        }
    };

    match cli.command {
        Some(Command::PrintOnce) => print_once(config.collector()),
        Some(Command::Check) => match MqttCredentials::load(config.mqtt()) {
            Ok(_) => {
                println!("Configuration and credential file are valid.");
                SUCCESS
            }
            Err(error) => {
                eprintln!("Configuration error: {error}");
                DIAGNOSTIC_ERROR
            }
        },
        None => match MqttCredentials::load(config.mqtt()) {
            Ok(_) => {
                eprintln!("The monitoring runtime is not available in this incremental build.");
                RUNTIME_ERROR
            }
            Err(error) => {
                eprintln!("Configuration error: {error}");
                CONFIG_ERROR
            }
        },
    }
}

fn print_once(config: &rpi_health_mqtt::config::CollectorConfig) -> u8 {
    let mut collector = match Collector::new(config) {
        Ok(collector) => collector,
        Err(error) => {
            eprintln!("Collection error: {error}");
            return rpi_health_mqtt::cli::COLLECTION_ERROR;
        }
    };
    match serde_json::to_string_pretty(&collector.collect()) {
        Ok(json) => {
            println!("{json}");
            SUCCESS
        }
        Err(error) => {
            eprintln!("Collection error: state serialization failed: {error}");
            rpi_health_mqtt::cli::COLLECTION_ERROR
        }
    }
}
