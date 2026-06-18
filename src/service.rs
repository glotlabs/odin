use std::time::{Duration, Instant};

use crate::config::{RestartPolicy, ServiceConfig};
use crate::status::{HealthStatus, ServiceState, ServiceStatus};

#[derive(Debug)]
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
}
