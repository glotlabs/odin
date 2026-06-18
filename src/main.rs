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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Monitor,
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
        Command::List => print_status(&cli.socket, None).await,
        Command::Status { service } => print_status(&cli.socket, service).await,
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
            _ = sighup.recv() => tracing::info!("SIGHUP received; config reload is not implemented yet"),
        }
    }

    supervisor.shutdown().await;
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    Ok(())
}

async fn print_status(socket: &std::path::Path, service: Option<String>) -> Result<()> {
    let response = control::request(socket, ControlRequest::Status { service }).await?;
    match response {
        ControlResponse::Status { services } => {
            for service in services {
                println!(
                    "{}\t{:?}\tpid={}\trestarts={}\thealth={:?}",
                    service.name,
                    service.state,
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
        ControlResponse::Error { message } => Err(SupperError::Protocol(message)),
        ControlResponse::Ok => Err(SupperError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}

async fn command_ok(socket: &std::path::Path, request: ControlRequest) -> Result<()> {
    match control::request(socket, request).await? {
        ControlResponse::Ok => Ok(()),
        ControlResponse::Error { message } => Err(SupperError::Protocol(message)),
        ControlResponse::Status { .. } => Err(SupperError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}
