use std::path::PathBuf;

use clap::{Parser, Subcommand};
use supper::config;
use supper::control::{self, ControlRequest, ControlResponse};
use supper::supervisor::SupervisorHandle;
use supper::{Result, SupperError};
use tokio::signal::unix::{SignalKind, signal};

const DEFAULT_CONFIG_DIR: &str = "/usr/local/etc/supper/services";
const DEFAULT_SOCKET: &str = "/var/run/supper.sock";

#[derive(Debug, Parser)]
#[command(name = "supper", version, about = "Minimal FreeBSD service supervisor")]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG_DIR, global = true)]
    config_dir: PathBuf,
    #[arg(long, default_value = DEFAULT_SOCKET, global = true)]
    socket: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Monitor,
    Validate,
    List,
    Status { service: Option<String> },
    Start { service: String },
    Stop { service: String },
    Restart { service: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Monitor) {
        Command::Monitor => monitor(cli.config_dir, cli.socket).await,
        Command::Validate => validate(cli.config_dir, cli.json),
        Command::List => print_status(&cli.socket, None, cli.json).await,
        Command::Status { service } => print_status(&cli.socket, service, cli.json).await,
        Command::Start { service } => {
            command_ok(&cli.socket, ControlRequest::Start { service }).await
        }
        Command::Stop { service } => {
            command_ok(&cli.socket, ControlRequest::Stop { service }).await
        }
        Command::Restart { service } => {
            command_ok(&cli.socket, ControlRequest::Restart { service }).await
        }
    }
}

fn validate(config_dir: PathBuf, json: bool) -> Result<()> {
    let report = config::validate_config_dir(&config_dir)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|err| SupperError::Protocol(err.to_string()))?
        );
    } else {
        println!("valid: {} service(s)", report.service_count);
        for warning in report.warnings {
            println!("warning: {}: {}", warning.service, warning.message);
        }
    }
    Ok(())
}

async fn monitor(config_dir: PathBuf, socket: PathBuf) -> Result<()> {
    let services = config::load_services(&config_dir)?;
    for service in &services {
        supper::logging::prepare_log_dirs(service)?;
    }
    let supervisor = SupervisorHandle::new(services);
    supervisor.start_autostart().await?;

    let control_supervisor = supervisor.clone();
    let control_socket = socket.clone();
    tokio::spawn(async move {
        if let Err(err) = control::serve(&control_socket, control_supervisor).await {
            tracing::error!("control API failed: {err}");
        }
    });

    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    loop {
        tokio::select! {
            _ = sigint.recv() => break,
            _ = sigterm.recv() => break,
            _ = sighup.recv() => {
                match config::load_services(&config_dir) {
                    Ok(services) => match supervisor.reload(services).await {
                        Ok(summary) => tracing::info!(
                            added = ?summary.added,
                            live_updated = ?summary.live_updated,
                            restarted = ?summary.restarted,
                            removed = ?summary.removed,
                            "configuration reloaded"
                        ),
                        Err(err) => tracing::error!("configuration reload failed: {err}"),
                    },
                    Err(err) => tracing::error!("configuration reload failed: {err}"),
                }
            },
        }
    }

    supervisor.shutdown().await;
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    Ok(())
}

async fn print_status(socket: &std::path::Path, service: Option<String>, json: bool) -> Result<()> {
    let response = control::request(socket, ControlRequest::Status { service }).await?;
    match response {
        ControlResponse::Status { services } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&services)
                        .map_err(|err| SupperError::Protocol(err.to_string()))?
                );
                return Ok(());
            }
            println!(
                "{:<24} {:<12} {:<8} {:<8} HEALTH",
                "NAME", "STATE", "PID", "RESTARTS"
            );
            for service in services {
                println!(
                    "{:<24} {:<12} {:<8} {:<8} {:?}",
                    service.name,
                    format!("{:?}", service.state),
                    service
                        .pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    service.restart_count,
                    service.health
                );
            }
            Ok(())
        }
        ControlResponse::Error { error } => Err(SupperError::Protocol(format!(
            "{}: {}",
            error.code, error.message
        ))),
        ControlResponse::Ok => Err(SupperError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}

async fn command_ok(socket: &std::path::Path, request: ControlRequest) -> Result<()> {
    match control::request(socket, request).await? {
        ControlResponse::Ok => Ok(()),
        ControlResponse::Error { error } => Err(SupperError::Protocol(format!(
            "{}: {}",
            error.code, error.message
        ))),
        ControlResponse::Status { .. } => Err(SupperError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}
