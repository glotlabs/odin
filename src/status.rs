use serde::{Deserialize, Serialize};

pub const MAX_RESTART_HISTORY: usize = 64;
pub const MAX_EVENT_HISTORY: usize = 128;

pub fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceEventKind {
    Started,
    Exited,
    Stopped,
    StopRequested,
    RestartScheduled,
    Restarted,
    HealthChanged,
    ReloadUpdated,
    ReloadRestartRequired,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEvent {
    pub at_unix_seconds: u64,
    pub kind: ServiceEventKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<u64>,
    pub restart_count: u64,
    pub last_exit: Option<String>,
    pub health: HealthStatus,
    pub restart_history: Vec<RestartHistoryEntry>,
    pub event_history: Vec<ServiceEvent>,
}
