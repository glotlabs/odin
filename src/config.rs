use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{OdinError, Result};
use crate::privileges::Privileges;

fn default_autostart() -> bool {
    true
}

fn default_restart() -> RestartPolicy {
    RestartPolicy::Always
}

fn default_initial_delay() -> Duration {
    Duration::from_secs(1)
}

fn default_max_delay() -> Duration {
    Duration::from_secs(30)
}

fn default_stop_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_startup_timeout() -> Duration {
    Duration::from_secs(2)
}

fn default_health_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_health_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_health_retries() -> u32 {
    3
}

fn default_health_action() -> HealthAction {
    HealthAction::Ignore
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthAction {
    Ignore,
    MarkUnready,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default = "default_autostart")]
    pub autostart: bool,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub umask: Option<u32>,
    #[serde(default = "default_restart")]
    pub restart: RestartPolicy,
    #[serde(default = "default_initial_delay", with = "humantime_serde")]
    pub restart_initial_delay: Duration,
    #[serde(default = "default_max_delay", with = "humantime_serde")]
    pub restart_max_delay: Duration,
    #[serde(default = "default_stop_timeout", with = "humantime_serde")]
    pub stop_timeout: Duration,
    #[serde(default = "default_startup_timeout", with = "humantime_serde")]
    pub startup_timeout: Duration,
    pub stdout_log: Option<PathBuf>,
    pub stderr_log: Option<PathBuf>,
    pub healthcheck: Option<HealthCheckConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    #[serde(rename = "type")]
    pub kind: HealthCheckKind,
    pub command: Option<PathBuf>,
    #[serde(default)]
    pub args: Vec<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub url: Option<String>,
    #[serde(default = "default_health_interval", with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default, with = "humantime_serde")]
    pub startup_grace: Duration,
    #[serde(default = "default_health_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    #[serde(default = "default_health_retries")]
    pub retries: u32,
    #[serde(default = "default_health_action")]
    pub action: HealthAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthCheckKind {
    Command,
    Tcp,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub service_count: usize,
    #[serde(default)]
    pub errors: Vec<ConfigDiagnostic>,
    pub warnings: Vec<ValidationIssue>,
    #[serde(default)]
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub service: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedService {
    pub path: PathBuf,
    pub service: ServiceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub severity: ConfigSeverity,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostics {
    pub service_count: usize,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigDiagnostics {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
            .count()
    }
}

impl std::fmt::Display for ConfigDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let error_count = self.error_count();
        let first = self
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
            .or_else(|| self.diagnostics.first());

        match (error_count, first) {
            (0, Some(first)) => write!(f, "configuration warning: {}", first.message),
            (1, Some(first)) => write!(f, "configuration error: {}", first.message),
            (count, Some(first)) => {
                write!(f, "{count} configuration errors; first: {}", first.message)
            }
            _ => write!(f, "configuration diagnostics"),
        }
    }
}

impl std::error::Error for ConfigDiagnostics {}

#[derive(Debug, Clone)]
struct LoadedService {
    path: PathBuf,
    service: ServiceConfig,
}

pub fn load_services(config_dir: &Path) -> Result<Vec<ServiceConfig>> {
    let (loaded, diagnostics) = collect_config_dir(config_dir, false)?;
    if diagnostics.has_errors() {
        return Err(OdinError::ConfigDiagnostics(diagnostics));
    }

    Ok(loaded.into_iter().map(|loaded| loaded.service).collect())
}

pub fn validate_config_dir(config_dir: &Path) -> Result<ValidationReport> {
    let (loaded, diagnostics) = collect_config_dir(config_dir, true)?;
    if diagnostics.has_errors() {
        return Err(OdinError::ConfigDiagnostics(diagnostics));
    }

    Ok(report_from_diagnostics(loaded.len(), diagnostics))
}

fn collect_config_dir(
    config_dir: &Path,
    include_runtime: bool,
) -> Result<(Vec<LoadedService>, ConfigDiagnostics)> {
    let mut loaded = Vec::new();
    let mut diagnostics = Vec::new();

    for entry in fs::read_dir(config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() != Some(OsStr::new("toml")) {
            continue;
        }
        let (service, mut file_diagnostics) = load_service_with_diagnostics(&path);
        diagnostics.append(&mut file_diagnostics);
        if let Some(service) = service {
            loaded.push(LoadedService { path, service });
        }
    }

    collect_unique_name_diagnostics(&loaded, &mut diagnostics);

    if include_runtime {
        for loaded_service in &loaded {
            collect_runtime_diagnostics(loaded_service, &mut diagnostics);
            collect_log_diagnostics(loaded_service, &mut diagnostics);
        }
    }

    let service_count = loaded.len();
    Ok((
        loaded,
        ConfigDiagnostics {
            service_count,
            diagnostics,
        },
    ))
}

pub fn load_service(path: &Path) -> Result<ServiceConfig> {
    let raw = fs::read_to_string(path)?;
    let service: ServiceConfig = toml::from_str(&raw).map_err(|source| OdinError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    validate_service(path, &service)?;
    Ok(service)
}

fn load_service_with_diagnostics(path: &Path) -> (Option<ServiceConfig>, Vec<ConfigDiagnostic>) {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            return (
                None,
                vec![diagnostic_with_location(
                    ConfigSeverity::Error,
                    path,
                    None,
                    None,
                    None,
                    None,
                    format!("failed to read config file: {err}"),
                    Some("Check that the file exists and is readable by odin.".to_string()),
                )],
            );
        }
    };

    let service: ServiceConfig = match toml::from_str(&raw) {
        Ok(service) => service,
        Err(source) => {
            let (line, column) = source
                .span()
                .map(|span| line_column(&raw, span.start))
                .unwrap_or((None, None));
            return (
                None,
                vec![diagnostic_with_location(
                    ConfigSeverity::Error,
                    path,
                    line,
                    column,
                    None,
                    None,
                    format!("TOML parse error: {source}"),
                    Some("Fix the TOML syntax or value type at this location.".to_string()),
                )],
            );
        }
    };

    let mut diagnostics = Vec::new();
    collect_service_diagnostics(path, &service, &mut diagnostics);
    (Some(service), diagnostics)
}

pub fn add_service_file(config_dir: &Path, name: &str) -> Result<AddedService> {
    validate_service_name(name)?;
    fs::create_dir_all(config_dir)?;
    let path = config_dir.join(format!("{name}.toml"));
    let service = derive_service_config(name);
    validate_service(&path, &service)?;

    let encoded = toml::to_string_pretty(&service)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::io::Write::write_all(&mut file, encoded.as_bytes())?;

    let loaded = load_service(&path)?;
    Ok(AddedService {
        path,
        service: loaded,
    })
}

pub fn derive_service_config(name: &str) -> ServiceConfig {
    ServiceConfig {
        name: name.to_string(),
        command: PathBuf::from(format!("/usr/local/bin/{name}")),
        args: Vec::new(),
        cwd: Some(PathBuf::from(format!("/usr/local/{name}"))),
        autostart: true,
        env: Default::default(),
        user: None,
        group: None,
        umask: None,
        restart: RestartPolicy::Always,
        restart_initial_delay: default_initial_delay(),
        restart_max_delay: default_max_delay(),
        stop_timeout: default_stop_timeout(),
        startup_timeout: default_startup_timeout(),
        stdout_log: Some(PathBuf::from(format!("/var/log/odin/{name}.out.log"))),
        stderr_log: Some(PathBuf::from(format!("/var/log/odin/{name}.err.log"))),
        healthcheck: None,
    }
}

fn validate_service(path: &Path, service: &ServiceConfig) -> Result<()> {
    let mut diagnostics = Vec::new();
    collect_service_diagnostics(path, service, &mut diagnostics);
    if let Some(first) = diagnostics
        .into_iter()
        .find(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
    {
        return Err(OdinError::InvalidConfig {
            path: first.path,
            message: first.message,
        });
    }
    Ok(())
}

fn collect_service_diagnostics(
    path: &Path,
    service: &ServiceConfig,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    collect_service_name_diagnostics(path, Some(&service.name), &service.name, diagnostics);
    if service.command.as_os_str().is_empty() {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            Some(&service.name),
            Some("command"),
            "command must not be empty",
            Some("Set command to an absolute executable path, for example \"/usr/local/bin/app\"."),
        ));
    }
    if let Some(mask) = service.umask
        && mask > 0o777
    {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            Some(&service.name),
            Some("umask"),
            "umask must be an octal permission mask no larger than 0777",
            Some("Use a value from 0 to 511 decimal, such as 18 for octal 022."),
        ));
    }
    if service.restart_initial_delay > service.restart_max_delay {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            Some(&service.name),
            Some("restart_initial_delay"),
            format!(
                "restart_initial_delay ({:?}) must be less than or equal to restart_max_delay ({:?})",
                service.restart_initial_delay, service.restart_max_delay
            ),
            Some("Increase restart_max_delay or lower restart_initial_delay.".to_string()),
        ));
    }

    if let Some(health) = &service.healthcheck {
        if health.retries == 0 {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                path,
                Some(&service.name),
                Some("healthcheck.retries"),
                "healthcheck retries must be greater than zero",
                Some("Set healthcheck.retries to 1 or more."),
            ));
        }
        match health.kind {
            HealthCheckKind::Command if health.command.is_none() => {
                diagnostics.push(diagnostic(
                    ConfigSeverity::Error,
                    path,
                    Some(&service.name),
                    Some("healthcheck.command"),
                    "command healthcheck requires command",
                    Some("Add healthcheck.command with an absolute executable path."),
                ));
            }
            HealthCheckKind::Tcp => {
                if health.host.is_none() {
                    diagnostics.push(diagnostic(
                        ConfigSeverity::Error,
                        path,
                        Some(&service.name),
                        Some("healthcheck.host"),
                        "tcp healthcheck requires host",
                        Some("Add healthcheck.host, for example \"127.0.0.1\"."),
                    ));
                }
                if health.port.is_none() {
                    diagnostics.push(diagnostic(
                        ConfigSeverity::Error,
                        path,
                        Some(&service.name),
                        Some("healthcheck.port"),
                        "tcp healthcheck requires port",
                        Some("Add healthcheck.port with the TCP port to probe."),
                    ));
                }
            }
            HealthCheckKind::Http if health.url.is_none() => {
                diagnostics.push(diagnostic(
                    ConfigSeverity::Error,
                    path,
                    Some(&service.name),
                    Some("healthcheck.url"),
                    "http healthcheck requires url",
                    Some("Add healthcheck.url, for example \"http://127.0.0.1:8080/health\"."),
                ));
            }
            _ => {}
        }
    }
}

fn validate_service_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(OdinError::InvalidConfig {
            path: PathBuf::from("<service-name>"),
            message: "name must not be empty".to_string(),
        });
    }
    if name.contains('/') || name.contains('\0') {
        return Err(OdinError::InvalidConfig {
            path: PathBuf::from("<service-name>"),
            message: "name must not contain '/' or NUL".to_string(),
        });
    }
    if name == "." || name == ".." {
        return Err(OdinError::InvalidConfig {
            path: PathBuf::from("<service-name>"),
            message: "name must not be '.' or '..'".to_string(),
        });
    }
    Ok(())
}

fn collect_runtime_diagnostics(loaded: &LoadedService, diagnostics: &mut Vec<ConfigDiagnostic>) {
    let service = &loaded.service;
    if !service.command.is_absolute() {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            &loaded.path,
            Some(&service.name),
            Some("command"),
            "command must be an absolute path",
            Some("Set command to an absolute path such as \"/usr/local/bin/app\"."),
        ));
    } else if !service.command.exists() {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            &loaded.path,
            Some(&service.name),
            Some("command"),
            format!("command does not exist: {}", service.command.display()),
            Some("Install the executable or update command to the correct absolute path."),
        ));
    }
    if let Some(cwd) = &service.cwd
        && !cwd.is_dir()
    {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            &loaded.path,
            Some(&service.name),
            Some("cwd"),
            format!(
                "cwd does not exist or is not a directory: {}",
                cwd.display()
            ),
            Some("Create the directory or update cwd to an existing directory."),
        ));
    }

    if let Err(err) = Privileges::resolve(service) {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            &loaded.path,
            Some(&service.name),
            Some("user"),
            format!(
                "privilege configuration is invalid for service {}: {err}",
                service.name
            ),
            Some("Check that configured user and group names exist on this system."),
        ));
    }

    if let Some(health) = &service.healthcheck
        && health.kind == HealthCheckKind::Command
        && let Some(command) = &health.command
    {
        if !command.is_absolute() {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                &loaded.path,
                Some(&service.name),
                Some("healthcheck.command"),
                "command healthcheck command must be an absolute path",
                Some("Set healthcheck.command to an absolute executable path."),
            ));
        } else if !command.exists() {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                &loaded.path,
                Some(&service.name),
                Some("healthcheck.command"),
                format!(
                    "command healthcheck command does not exist: {}",
                    command.display()
                ),
                Some("Install the healthcheck executable or update healthcheck.command."),
            ));
        }
    }
}

fn collect_log_diagnostics(loaded: &LoadedService, diagnostics: &mut Vec<ConfigDiagnostic>) {
    let service = &loaded.service;
    for (field, path) in [
        ("stdout_log", &service.stdout_log),
        ("stderr_log", &service.stderr_log),
    ]
    .into_iter()
    .filter_map(|(field, path)| path.as_ref().map(|path| (field, path)))
    {
        if !path.is_absolute() {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                &loaded.path,
                Some(&service.name),
                Some(field),
                format!("log path must be absolute: {}", path.display()),
                Some("Use an absolute log path, for example \"/var/log/odin/app.out.log\"."),
            ));
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent.exists() && !parent.is_dir() {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                &loaded.path,
                Some(&service.name),
                Some(field),
                format!("log parent is not a directory: {}", parent.display()),
                Some("Point the log path at a file inside an existing directory."),
            ));
        }
        if !parent.exists() {
            diagnostics.push(diagnostic(
                ConfigSeverity::Warning,
                &loaded.path,
                Some(&service.name),
                Some(field),
                format!(
                    "log directory does not exist yet and will be created by monitor: {}",
                    parent.display()
                ),
                Some("No action is required if the monitor user can create this directory."),
            ));
        }
    }
}

fn collect_unique_name_diagnostics(
    loaded: &[LoadedService],
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let mut seen: HashMap<&str, &LoadedService> = HashMap::new();
    for service in loaded {
        if let Some(first) = seen.insert(&service.service.name, service) {
            diagnostics.push(diagnostic(
                ConfigSeverity::Error,
                &service.path,
                Some(&service.service.name),
                Some("name"),
                format!(
                    "duplicate service name {:?} in {} and {}",
                    service.service.name,
                    first.path.display(),
                    service.path.display()
                ),
                Some("Give each service a unique name across the config directory."),
            ));
        }
    }
}

fn collect_service_name_diagnostics(
    path: &Path,
    service: Option<&str>,
    name: &str,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    if name.trim().is_empty() {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            service,
            Some("name"),
            "name must not be empty",
            Some("Set name to a non-empty service identifier."),
        ));
    }
    if name.contains('/') || name.contains('\0') {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            service,
            Some("name"),
            "name must not contain '/' or NUL",
            Some("Use a plain service name such as \"web\" or \"worker-1\"."),
        ));
    }
    if name == "." || name == ".." {
        diagnostics.push(diagnostic(
            ConfigSeverity::Error,
            path,
            service,
            Some("name"),
            "name must not be '.' or '..'",
            Some("Use a plain service name such as \"web\" or \"worker-1\"."),
        ));
    }
}

fn report_from_diagnostics(
    service_count: usize,
    diagnostics: ConfigDiagnostics,
) -> ValidationReport {
    let warnings = diagnostics
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Warning)
        .map(|diagnostic| ValidationIssue {
            service: diagnostic
                .service
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            message: diagnostic.message.clone(),
        })
        .collect();

    ValidationReport {
        service_count,
        errors: Vec::new(),
        warnings,
        diagnostics: diagnostics.diagnostics,
    }
}

fn diagnostic(
    severity: ConfigSeverity,
    path: &Path,
    service: Option<&str>,
    field: Option<&str>,
    message: impl Into<String>,
    help: Option<impl Into<String>>,
) -> ConfigDiagnostic {
    diagnostic_with_location(severity, path, None, None, service, field, message, help)
}

fn diagnostic_with_location(
    severity: ConfigSeverity,
    path: &Path,
    line: Option<usize>,
    column: Option<usize>,
    service: Option<&str>,
    field: Option<&str>,
    message: impl Into<String>,
    help: Option<impl Into<String>>,
) -> ConfigDiagnostic {
    ConfigDiagnostic {
        severity,
        path: path.to_path_buf(),
        line,
        column,
        service: service.map(ToString::to_string),
        field: field.map(ToString::to_string),
        message: message.into(),
        help: help.map(Into::into),
    }
}

fn line_column(input: &str, byte_offset: usize) -> (Option<usize>, Option<usize>) {
    let mut line = 1;
    let mut column = 1;
    for (index, ch) in input.char_indices() {
        if index >= byte_offset {
            return (Some(line), Some(column));
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (Some(line), Some(column))
}
