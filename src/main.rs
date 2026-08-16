//! Command-line entry point for the monitoring service.

use std::process::ExitCode;

use clap::Parser;
use rpi_health_mqtt::{
    cli::{Cli, Command, CONFIG_ERROR, DIAGNOSTIC_ERROR, RUNTIME_ERROR, SUCCESS},
    collector::Collector,
    config::{Config, MqttCredentials},
    daemon,
    diagnostics::{self, MqttProbe},
};

fn main() -> ExitCode {
    daemon::initialize_logging();
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
        Some(Command::Check { skip_mqtt }) => run_diagnostics(&config, skip_mqtt),
        None => match MqttCredentials::load(config.mqtt()) {
            Ok(credentials) => match daemon::run(config, credentials) {
                Ok(()) => SUCCESS,
                Err(error) => {
                    eprintln!("Runtime error: {error}");
                    RUNTIME_ERROR
                }
            },
            Err(error) => {
                eprintln!("Configuration error: {error}");
                CONFIG_ERROR
            }
        },
    }
}

fn run_diagnostics(config: &Config, skip_mqtt: bool) -> u8 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("[FAILURE] runtime: diagnostic runtime could not start");
            return DIAGNOSTIC_ERROR;
        }
    };
    let mqtt_probe = if skip_mqtt {
        MqttProbe::Skipped
    } else {
        MqttProbe::Enabled
    };
    let report = runtime.block_on(diagnostics::run_checks(config, mqtt_probe));
    println!("{report}");
    report.exit_code()
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
