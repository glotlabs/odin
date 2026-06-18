use serde::{Deserialize, Serialize};

pub const MAX_RESTART_HISTORY: usize = 64;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartReason {
    Automatic,
    Manual,
    Reload,
    HealthCheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartHistoryEntry {
    pub at_unix_seconds: u64,
    pub reason: RestartReason,
    pub from_pid: Option<u32>,
    pub to_pid: Option<u32>,
    pub exit: Option<String>,
    pub backoff_millis: Option<u64>,
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
    pub restart_history: Vec<RestartHistoryEntry>,
}
