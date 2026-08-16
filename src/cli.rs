//! Stable command-line contract and process exit codes.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Default location of the service configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/rpi-health-mqtt/config.toml";
/// Successful process completion.
pub const SUCCESS: u8 = 0;
/// Invalid command-line syntax.
pub const USAGE_ERROR: u8 = 2;
/// Invalid configuration or credential file.
pub const CONFIG_ERROR: u8 = 3;
/// A blocking diagnostic check failed.
pub const DIAGNOSTIC_ERROR: u8 = 4;
/// A one-shot collection failed.
pub const COLLECTION_ERROR: u8 = 5;
/// The daemon could not start or encountered an unrecoverable error.
pub const RUNTIME_ERROR: u8 = 6;

/// Parsed command-line arguments.
#[derive(Clone, Debug, Parser, PartialEq, Eq)]
#[command(
    name = "rpi-health-mqtt",
    version,
    about = "Publish Raspberry Pi health metrics over MQTT"
)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    pub config: PathBuf,

    /// Optional diagnostic command; omission starts the daemon.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Non-daemon commands.
#[derive(Clone, Copy, Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Validate configuration, credentials, dependencies, and connectivity.
    Check {
        /// Skip the bounded MQTT connectivity probe.
        #[arg(long)]
        skip_mqtt: bool,
    },
    /// Collect one snapshot as JSON without connecting to MQTT.
    PrintOnce,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_selects_daemon_and_default_config() {
        let cli = Cli::try_parse_from(["rpi-health-mqtt"]).expect("arguments should parse");
        assert_eq!(cli.config, PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(cli.command, None);
    }

    #[test]
    fn global_config_is_accepted_before_or_after_subcommand() {
        for arguments in [
            ["rpi-health-mqtt", "--config", "/tmp/example.toml", "check"],
            ["rpi-health-mqtt", "check", "--config", "/tmp/example.toml"],
        ] {
            let cli = Cli::try_parse_from(arguments).expect("arguments should parse");
            assert_eq!(cli.config, PathBuf::from("/tmp/example.toml"));
            assert_eq!(cli.command, Some(Command::Check { skip_mqtt: false }));
        }
    }

    #[test]
    fn check_can_skip_only_the_mqtt_probe() {
        let cli = Cli::try_parse_from(["rpi-health-mqtt", "check", "--skip-mqtt"])
            .expect("check arguments should parse");

        assert_eq!(cli.command, Some(Command::Check { skip_mqtt: true }));
    }

    #[test]
    fn diagnostic_commands_and_usage_errors_are_stable() {
        let print = Cli::try_parse_from(["rpi-health-mqtt", "print-once"])
            .expect("print-once should parse");
        assert_eq!(print.command, Some(Command::PrintOnce));

        for arguments in [
            vec!["rpi-health-mqtt", "unknown"],
            vec!["rpi-health-mqtt", "--config"],
            vec!["rpi-health-mqtt", "check", "extra"],
        ] {
            let error = Cli::try_parse_from(arguments).expect_err("arguments must fail");
            assert_eq!(error.exit_code(), i32::from(USAGE_ERROR));
        }
    }

    #[test]
    fn help_and_version_are_successful_control_flow() {
        for argument in ["--help", "--version"] {
            let error = Cli::try_parse_from(["rpi-health-mqtt", argument])
                .expect_err("clap represents display output as an error");
            assert!(!error.use_stderr());
            let text = error.to_string();
            assert!(!text.contains("password"));
            assert!(!text.contains("environment"));
        }
    }
}
