use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    BackingOff,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    #[default]
    Unknown,
    Healthy,
    Unhealthy(String),
    Unready(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<u64>,
    pub restart_count: u64,
    pub last_exit: Option<String>,
    pub health: HealthStatus,
}
