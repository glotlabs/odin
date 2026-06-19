use std::path::PathBuf;

use clap::{Parser, Subcommand};
use odin::config::{self, ConfigDiagnostic, ConfigDiagnostics, ConfigSeverity, ValidationReport};
use odin::control::{self, ControlRequest, ControlResponse};
use odin::labels::json_display;
use odin::status::ServiceEvent;
use odin::supervisor::SupervisorHandle;
use odin::{OdinError, Result};
use tokio::signal::unix::{SignalKind, signal};

const DEFAULT_CONFIG_DIR: &str = "/usr/local/etc/odin/services";
const DEFAULT_SOCKET: &str = "/var/run/odin.sock";

#[derive(Debug, Parser)]
#[command(name = "odin", version, about = "Minimal FreeBSD service supervisor")]
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
    /// Run the foreground supervisor
    Serve,
    /// Create a starter service config
    Add { name: String },
    /// Validate service configs without starting services
    Validate,
    /// Reload the supervisor config over the control socket
    Reload,
    /// Show service status
    Status { service: Option<String> },
    /// Show recent lifecycle events for a service
    Events { service: String },
    /// Start a service
    Start { service: String },
    /// Stop a service
    Stop { service: String },
    /// Restart a service
    Restart { service: String },
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        if let OdinError::ConfigDiagnostics(diagnostics) = &err {
            print_diagnostics(&diagnostics.diagnostics);
        } else {
            eprintln!("error: {err}");
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(cli.config_dir, cli.socket).await,
        Command::Add { name } => add(cli.config_dir, &name, cli.json),
        Command::Validate => validate(cli.config_dir, cli.json),
        Command::Reload => reload(&cli.socket, cli.json).await,
        Command::Status { service } => print_status(&cli.socket, service, cli.json).await,
        Command::Events { service } => print_events(&cli.socket, &service, cli.json).await,
        Command::Start { service } => {
            command_ok(&cli.socket, ControlRequest::Start { service }, cli.json).await
        }
        Command::Stop { service } => {
            command_ok(&cli.socket, ControlRequest::Stop { service }, cli.json).await
        }
        Command::Restart { service } => {
            command_ok(&cli.socket, ControlRequest::Restart { service }, cli.json).await
        }
    }
}

fn add(config_dir: PathBuf, name: &str, json: bool) -> Result<()> {
    let added = config::add_service_file(&config_dir, name)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&added)
                .map_err(|err| OdinError::Protocol(err.to_string()))?
        );
    } else {
        println!("created {}", added.path.display());
        println!("run `odin reload` to apply the new service");
    }
    Ok(())
}

fn validate(config_dir: PathBuf, json: bool) -> Result<()> {
    let report = match config::validate_config_dir(&config_dir) {
        Ok(report) => report,
        Err(OdinError::ConfigDiagnostics(diagnostics)) => {
            let report = validation_report_from_diagnostics(diagnostics);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
            } else {
                print_diagnostics(&report.diagnostics);
            }
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|err| OdinError::Protocol(err.to_string()))?
        );
    } else {
        println!("valid: {} service(s)", report.service_count);
        for warning in report.warnings {
            println!("warning: {}: {}", warning.service, warning.message);
        }
    }
    Ok(())
}

fn validation_report_from_diagnostics(diagnostics: ConfigDiagnostics) -> ValidationReport {
    let errors = diagnostics
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
        .cloned()
        .collect();
    let warnings = diagnostics
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Warning)
        .map(|diagnostic| config::ValidationIssue {
            service: diagnostic
                .service
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            message: diagnostic.message.clone(),
        })
        .collect();

    ValidationReport {
        service_count: diagnostics.service_count,
        errors,
        warnings,
        diagnostics: diagnostics.diagnostics,
    }
}

fn print_diagnostics(diagnostics: &[ConfigDiagnostic]) {
    for diagnostic in diagnostics {
        let severity = match diagnostic.severity {
            ConfigSeverity::Error => "error",
            ConfigSeverity::Warning => "warning",
        };
        let mut location = diagnostic.path.display().to_string();
        if let Some(line) = diagnostic.line {
            location.push(':');
            location.push_str(&line.to_string());
            if let Some(column) = diagnostic.column {
                location.push(':');
                location.push_str(&column.to_string());
            }
        }
        if let Some(field) = &diagnostic.field {
            location.push(' ');
            location.push_str(field);
        }

        eprintln!("{severity}: {location}: {}", diagnostic.message);
        if let Some((line_number, line)) = diagnostic_source_line(diagnostic) {
            eprintln!("{line_number:>6} | {line}");
            if let Some(column) = diagnostic.column {
                let caret_offset = line
                    .chars()
                    .take(column.saturating_sub(1))
                    .map(|ch| if ch == '\t' { '\t' } else { ' ' })
                    .collect::<String>();
                eprintln!("       | {caret_offset}^");
            }
        }
        if let Some(help) = &diagnostic.help {
            eprintln!("  help: {help}");
        }
    }
}

fn diagnostic_source_line(diagnostic: &ConfigDiagnostic) -> Option<(usize, String)> {
    let line_number = diagnostic.line?;
    let raw = std::fs::read_to_string(&diagnostic.path).ok()?;
    raw.lines()
        .nth(line_number.saturating_sub(1))
        .map(|line| (line_number, line.to_string()))
}

async fn serve(config_dir: PathBuf, socket: PathBuf) -> Result<()> {
    odin::logging::detach_process_stdio()?;
    let services = config::load_services(&config_dir)?;
    for service in &services {
        odin::logging::prepare_log_dirs(service)?;
    }
    let supervisor = SupervisorHandle::new(services);
    supervisor.start_autostart().await?;

    let control_supervisor = supervisor.clone();
    let control_socket = socket.clone();
    let control_config_dir = config_dir.clone();
    tokio::spawn(async move {
        if let Err(err) =
            control::serve(&control_socket, control_config_dir, control_supervisor).await
        {
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
                        Err(err) => log_config_error("configuration reload failed", &err),
                    },
                    Err(err) => log_config_error("configuration reload failed", &err),
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

fn log_config_error(context: &str, err: &OdinError) {
    if let OdinError::ConfigDiagnostics(diagnostics) = err {
        for diagnostic in &diagnostics.diagnostics {
            tracing::error!(
                path = %diagnostic.path.display(),
                field = diagnostic.field.as_deref(),
                service = diagnostic.service.as_deref(),
                help = diagnostic.help.as_deref(),
                "{context}: {}",
                diagnostic.message
            );
        }
    } else {
        tracing::error!("{context}: {err}");
    }
}

async fn reload(socket: &std::path::Path, json: bool) -> Result<()> {
    let response = control::request(socket, ControlRequest::Reload).await?;
    match response {
        ControlResponse::Reload { summary } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
            } else {
                println!(
                    "reloaded: added={}, live-updated={}, restarted={}, removed={}",
                    summary.added.len(),
                    summary.live_updated.len(),
                    summary.restarted.len(),
                    summary.removed.len()
                );
            }
            Ok(())
        }
        ControlResponse::Error { error } => {
            if let Some(diagnostics) = &error.config_diagnostics {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&error)
                            .map_err(|err| OdinError::Protocol(err.to_string()))?
                    );
                } else {
                    print_diagnostics(diagnostics);
                }
                std::process::exit(1);
            }
            Err(OdinError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            )))
        }
        ControlResponse::Status { .. }
        | ControlResponse::Ok
        | ControlResponse::Operation { .. } => Err(OdinError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}

async fn print_status(socket: &std::path::Path, service: Option<String>, json: bool) -> Result<()> {
    let response = control::request(socket, ControlRequest::Status { service }).await?;
    match response {
        ControlResponse::Status { services } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&services)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
                return Ok(());
            }
            println!(
                "{:<24} {:<12} {:<8} {:<10} {:<8} {:<16} HEALTH",
                "NAME", "STATE", "PID", "UPTIME", "RESTARTS", "LAST-RESTART"
            );
            for service in services {
                println!(
                    "{:<24} {:<12} {:<8} {:<10} {:<8} {:<16} {}",
                    service.name,
                    json_display(service.state),
                    service
                        .pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    service
                        .uptime_seconds
                        .map(format_duration)
                        .unwrap_or_else(|| "-".to_string()),
                    service.restart_count,
                    service
                        .restart_history
                        .last()
                        .map(|entry| json_display(entry.reason))
                        .unwrap_or_else(|| "-".to_string()),
                    json_display(service.health)
                );
            }
            Ok(())
        }
        ControlResponse::Error { error } => Err(OdinError::Protocol(format!(
            "{}: {}",
            error.code, error.message
        ))),
        ControlResponse::Ok
        | ControlResponse::Reload { .. }
        | ControlResponse::Operation { .. } => Err(OdinError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}

fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        format!("{}d{}h", days, hours % 24)
    } else if hours > 0 {
        format!("{}h{}m", hours, minutes % 60)
    } else if minutes > 0 {
        format!("{}m{}s", minutes, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

async fn print_events(socket: &std::path::Path, service: &str, json: bool) -> Result<()> {
    let response = control::request(
        socket,
        ControlRequest::Status {
            service: Some(service.to_string()),
        },
    )
    .await?;
    match response {
        ControlResponse::Status { services } => {
            let service = services
                .into_iter()
                .next()
                .ok_or_else(|| OdinError::Protocol("missing service status".to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&service.event_history)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
                return Ok(());
            }
            print_event_table(&service.event_history);
            Ok(())
        }
        ControlResponse::Error { error } => Err(OdinError::Protocol(format!(
            "{}: {}",
            error.code, error.message
        ))),
        ControlResponse::Ok
        | ControlResponse::Reload { .. }
        | ControlResponse::Operation { .. } => Err(OdinError::Protocol(
            "unexpected control response".to_string(),
        )),
    }
}

fn print_event_table(events: &[ServiceEvent]) {
    println!("{:<12} {:<24} MESSAGE", "TIME", "KIND");
    for event in events {
        println!(
            "{:<12} {:<24} {}",
            event.at_unix_seconds,
            json_display(event.kind),
            event.message
        );
    }
}

async fn command_ok(socket: &std::path::Path, request: ControlRequest, json: bool) -> Result<()> {
    match control::request(socket, request).await? {
        ControlResponse::Operation { result } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
                return Ok(());
            }
            println!(
                "{}: {} (action={}, state={}, pid={})",
                result.service,
                result.message,
                json_display(result.action),
                json_display(result.status.state),
                result
                    .status
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            if let Some(last_exit) = result.status.last_exit {
                println!("last exit: {last_exit}");
            }
            Ok(())
        }
        ControlResponse::Ok => Ok(()),
        ControlResponse::Error { error } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&error)
                        .map_err(|err| OdinError::Protocol(err.to_string()))?
                );
                std::process::exit(1);
            }
            print_control_error_details(&error);
            Err(OdinError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            )))
        }
        ControlResponse::Status { .. } | ControlResponse::Reload { .. } => Err(
            OdinError::Protocol("unexpected control response".to_string()),
        ),
    }
}

fn print_control_error_details(error: &odin::control::ControlError) {
    if let Some(operation) = &error.operation {
        eprintln!(
            "{}: action={}, phase={}, pid={}, state={}, timeout_ms={}",
            operation.service,
            json_display(operation.action),
            json_display(operation.phase),
            operation
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_string()),
            operation
                .state
                .map(json_display)
                .unwrap_or_else(|| "-".to_string()),
            operation
                .timeout_millis
                .map(|timeout| timeout.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
        eprintln!("operation: {}", operation.message);
    }
    let Some(status) = &error.status else {
        return;
    };
    eprintln!(
        "{}: state={}, pid={}, restarts={}",
        status.name,
        json_display(status.state),
        status
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string()),
        status.restart_count
    );
    if let Some(last_exit) = &status.last_exit {
        eprintln!("last exit: {last_exit}");
    }
    let event_count = status.event_history.len();
    let start = event_count.saturating_sub(5);
    for event in &status.event_history[start..] {
        eprintln!(
            "event: {} {}: {}",
            event.at_unix_seconds,
            json_display(event.kind),
            event.message
        );
    }
}
