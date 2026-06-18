use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::config::{RestartPolicy, ServiceConfig};
use crate::status::{
    HealthStatus, MAX_EVENT_HISTORY, MAX_RESTART_HISTORY, RestartHistoryEntry, ServiceEvent,
    ServiceEventKind, ServiceState, ServiceStatus,
};

pub struct ServiceRuntime {
    pub config: ServiceConfig,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub desired_running: bool,
    pub started_at: Option<Instant>,
    pub restart_count: u64,
    pub generation: u64,
    pub last_exit: Option<String>,
    pub current_backoff: Duration,
    pub health: HealthStatus,
    pub health_failures: u32,
    pub stop_waiters: Vec<oneshot::Sender<()>>,
    pub restart_history: VecDeque<RestartHistoryEntry>,
    pub event_history: VecDeque<ServiceEvent>,
}

impl ServiceRuntime {
    pub fn new(config: ServiceConfig) -> Self {
        let desired_running = config.autostart;
        let current_backoff = config.restart_initial_delay;
        Self {
            config,
            state: ServiceState::Stopped,
            pid: None,
            desired_running,
            started_at: None,
            restart_count: 0,
            generation: 0,
            last_exit: None,
            current_backoff,
            health: HealthStatus::Unknown,
            health_failures: 0,
            stop_waiters: Vec::new(),
            restart_history: VecDeque::new(),
            event_history: VecDeque::new(),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            name: self.config.name.clone(),
            state: self.state,
            pid: self.pid,
            uptime_seconds: self.started_at.map(|t| t.elapsed().as_secs()),
            restart_count: self.restart_count,
            last_exit: self.last_exit.clone(),
            health: self.health.clone(),
            restart_history: self.restart_history.iter().cloned().collect(),
            event_history: self.event_history.iter().cloned().collect(),
        }
    }

    pub fn should_restart(&self, successful_exit: bool) -> bool {
        if !self.desired_running {
            return false;
        }

        match self.config.restart {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => !successful_exit,
            RestartPolicy::Always => true,
        }
    }

    pub fn advance_backoff(&mut self) -> Duration {
        let delay = self.current_backoff;
        self.current_backoff = (self.current_backoff * 2).min(self.config.restart_max_delay);
        delay
    }

    pub fn reset_backoff(&mut self) {
        self.current_backoff = self.config.restart_initial_delay;
    }

    pub fn notify_stopped(&mut self) {
        for waiter in self.stop_waiters.drain(..) {
            let _ = waiter.send(());
        }
    }

    pub fn record_restart(&mut self, entry: RestartHistoryEntry) {
        if self.restart_history.len() == MAX_RESTART_HISTORY {
            self.restart_history.pop_front();
        }
        self.restart_history.push_back(entry);
    }

    pub fn record_event(&mut self, kind: ServiceEventKind, message: impl Into<String>) {
        if self.event_history.len() == MAX_EVENT_HISTORY {
            self.event_history.pop_front();
        }
        self.event_history.push_back(ServiceEvent {
            at_unix_seconds: crate::status::now_unix_seconds(),
            kind,
            message: message.into(),
        });
    }
}
