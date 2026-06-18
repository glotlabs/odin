use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SupperError};
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
    pub warnings: Vec<ValidationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub service: String,
    pub message: String,
}

pub fn load_services(config_dir: &Path) -> Result<Vec<ServiceConfig>> {
    let mut services = Vec::new();
    for entry in fs::read_dir(config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() != Some(OsStr::new("toml")) {
            continue;
        }
        services.push(load_service(&path)?);
    }
    validate_unique_names(&services)?;
    Ok(services)
}

pub fn validate_config_dir(config_dir: &Path) -> Result<ValidationReport> {
    let services = load_services(config_dir)?;
    let mut warnings = Vec::new();

    for service in &services {
        validate_runtime_inputs(service)?;
        collect_log_warnings(service, &mut warnings)?;
    }

    Ok(ValidationReport {
        service_count: services.len(),
        warnings,
    })
}

pub fn load_service(path: &Path) -> Result<ServiceConfig> {
    let raw = fs::read_to_string(path)?;
    let service: ServiceConfig = toml::from_str(&raw).map_err(|source| SupperError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    validate_service(path, &service)?;
    Ok(service)
}

fn validate_unique_names(services: &[ServiceConfig]) -> Result<()> {
    let mut names = HashSet::new();
    for service in services {
        if !names.insert(service.name.clone()) {
            return Err(SupperError::DuplicateService(service.name.clone()));
        }
    }
    Ok(())
}

fn validate_service(path: &Path, service: &ServiceConfig) -> Result<()> {
    let invalid = |message: &str| SupperError::InvalidConfig {
        path: path.to_path_buf(),
        message: message.to_string(),
    };

    if service.name.trim().is_empty() {
        return Err(invalid("name must not be empty"));
    }
    if service.name.contains('/') || service.name.contains('\0') {
        return Err(invalid("name must not contain '/' or NUL"));
    }
    if service.command.as_os_str().is_empty() {
        return Err(invalid("command must not be empty"));
    }
    if let Some(mask) = service.umask
        && mask > 0o777
    {
        return Err(invalid(
            "umask must be an octal permission mask no larger than 0777",
        ));
    }
    if service.restart_initial_delay > service.restart_max_delay {
        return Err(invalid(
            "restart_initial_delay must be less than or equal to restart_max_delay",
        ));
    }

    if let Some(health) = &service.healthcheck {
        if health.retries == 0 {
            return Err(invalid("healthcheck retries must be greater than zero"));
        }
        match health.kind {
            HealthCheckKind::Command if health.command.is_none() => {
                return Err(invalid("command healthcheck requires command"));
            }
            HealthCheckKind::Tcp if health.host.is_none() || health.port.is_none() => {
                return Err(invalid("tcp healthcheck requires host and port"));
            }
            HealthCheckKind::Http if health.url.is_none() => {
                return Err(invalid("http healthcheck requires url"));
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_runtime_inputs(service: &ServiceConfig) -> Result<()> {
    let invalid = |message: String| SupperError::InvalidConfig {
        path: PathBuf::from(format!("<service:{}>", service.name)),
        message,
    };

    if !service.command.is_absolute() {
        return Err(invalid("command must be an absolute path".to_string()));
    }
    if !service.command.exists() {
        return Err(invalid(format!(
            "command does not exist: {}",
            service.command.display()
        )));
    }
    if let Some(cwd) = &service.cwd
        && !cwd.is_dir()
    {
        return Err(invalid(format!(
            "cwd does not exist or is not a directory: {}",
            cwd.display()
        )));
    }

    Privileges::resolve(service).map_err(|err| {
        invalid(format!(
            "privilege configuration is invalid for service {}: {err}",
            service.name
        ))
    })?;

    if let Some(health) = &service.healthcheck
        && health.kind == HealthCheckKind::Command
        && let Some(command) = &health.command
    {
        if !command.is_absolute() {
            return Err(invalid(
                "command healthcheck command must be an absolute path".to_string(),
            ));
        }
        if !command.exists() {
            return Err(invalid(format!(
                "command healthcheck command does not exist: {}",
                command.display()
            )));
        }
    }

    Ok(())
}

fn collect_log_warnings(
    service: &ServiceConfig,
    warnings: &mut Vec<ValidationIssue>,
) -> Result<()> {
    for path in [&service.stdout_log, &service.stderr_log]
        .into_iter()
        .flatten()
    {
        if !path.is_absolute() {
            return Err(SupperError::InvalidConfig {
                path: PathBuf::from(format!("<service:{}>", service.name)),
                message: format!("log path must be absolute: {}", path.display()),
            });
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent.exists() && !parent.is_dir() {
            return Err(SupperError::InvalidConfig {
                path: PathBuf::from(format!("<service:{}>", service.name)),
                message: format!("log parent is not a directory: {}", parent.display()),
            });
        }
        if !parent.exists() {
            warnings.push(ValidationIssue {
                service: service.name.clone(),
                message: format!(
                    "log directory does not exist yet and will be created by monitor: {}",
                    parent.display()
                ),
            });
        }
    }
    Ok(())
}
