use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SupperError};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    if service.restart_initial_delay > service.restart_max_delay {
        return Err(invalid(
            "restart_initial_delay must be less than or equal to restart_max_delay",
        ));
    }

    if let Some(health) = &service.healthcheck {
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
