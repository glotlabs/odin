use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::time;

use crate::child;
use crate::config::{HealthAction, ServiceConfig};
use crate::error::{Result, SupperError};
use crate::service::ServiceRuntime;
use crate::status::{HealthStatus, ServiceState, ServiceStatus};

#[derive(Clone)]
pub struct SupervisorHandle {
    inner: Arc<Mutex<BTreeMap<String, ServiceRuntime>>>,
}

impl SupervisorHandle {
    pub fn new(services: Vec<ServiceConfig>) -> Self {
        let mut map = BTreeMap::new();
        for service in services {
            map.insert(service.name.clone(), ServiceRuntime::new(service));
        }
        Self {
            inner: Arc::new(Mutex::new(map)),
        }
    }

    fn services(&self) -> Result<MutexGuard<'_, BTreeMap<String, ServiceRuntime>>> {
        self.inner
            .lock()
            .map_err(|_| SupperError::Protocol("supervisor state lock poisoned".to_string()))
    }

    pub async fn start_autostart(&self) -> Result<()> {
        let names = {
            let services = self.services()?;
            services
                .values()
                .filter(|service| service.config.autostart)
                .map(|service| service.config.name.clone())
                .collect::<Vec<_>>()
        };
        for name in names {
            self.start(&name).await?;
        }
        Ok(())
    }

    pub async fn status(&self, service: Option<&str>) -> Result<Vec<ServiceStatus>> {
        let services = self.services()?;
        if let Some(name) = service {
            let service = services
                .get(name)
                .ok_or_else(|| SupperError::ServiceNotFound(name.to_string()))?;
            return Ok(vec![service.status()]);
        }
        Ok(services.values().map(ServiceRuntime::status).collect())
    }

    pub async fn start(&self, name: &str) -> Result<()> {
        let config = {
            let mut services = self.services()?;
            let service = services
                .get_mut(name)
                .ok_or_else(|| SupperError::ServiceNotFound(name.to_string()))?;
            if matches!(
                service.state,
                ServiceState::Running | ServiceState::Starting | ServiceState::Stopping
            ) {
                return Err(SupperError::AlreadyRunning(name.to_string()));
            }
            service.desired_running = true;
            service.state = ServiceState::Starting;
            service.config.clone()
        };
        self.spawn_and_track(config, false)
    }

    pub async fn stop(&self, name: &str) -> Result<()> {
        let (pid, timeout) = {
            let mut services = self.services()?;
            let service = services
                .get_mut(name)
                .ok_or_else(|| SupperError::ServiceNotFound(name.to_string()))?;
            service.desired_running = false;
            service.state = ServiceState::Stopping;
            (service.pid, service.config.stop_timeout)
        };

        let pid = pid.ok_or_else(|| SupperError::NotRunning(name.to_string()))?;
        terminate_process_group(pid, timeout).await;
        Ok(())
    }

    pub async fn restart(&self, name: &str) -> Result<()> {
        let _ = self.stop(name).await;
        let timeout = {
            let services = self.services()?;
            services
                .get(name)
                .ok_or_else(|| SupperError::ServiceNotFound(name.to_string()))?
                .config
                .stop_timeout
        };
        time::sleep(timeout.min(Duration::from_secs(1))).await;
        self.start(name).await
    }

    pub async fn shutdown(&self) {
        let names = {
            let Ok(services) = self.services() else {
                return;
            };
            services.keys().cloned().collect::<Vec<_>>()
        };
        for name in names {
            let _ = self.stop(&name).await;
        }
    }

    fn spawn_and_track(&self, config: ServiceConfig, count_restart: bool) -> Result<()> {
        let mut child = child::spawn_service(&config)?;
        let pid = child.id();
        let generation;
        {
            let mut services = self.services()?;
            let service = services
                .get_mut(&config.name)
                .ok_or_else(|| SupperError::ServiceNotFound(config.name.clone()))?;
            service.state = ServiceState::Running;
            service.pid = pid;
            service.generation += 1;
            generation = service.generation;
            service.started_at = Some(std::time::Instant::now());
            service.last_exit = None;
            service.health = HealthStatus::Unknown;
            service.health_failures = 0;
            service.reset_backoff();
            if count_restart {
                service.restart_count += 1;
            }
        }

        let handle = self.clone();
        let name = config.name.clone();
        tokio::spawn(async move {
            let exit = child.wait().await;
            handle.handle_exit(&name, generation, pid, exit).await;
        });

        if config.healthcheck.is_some() {
            let handle = self.clone();
            tokio::spawn(async move {
                handle.health_loop(config.name, generation).await;
            });
        }

        Ok(())
    }

    async fn handle_exit(
        &self,
        name: &str,
        generation: u64,
        pid: Option<u32>,
        exit: std::io::Result<std::process::ExitStatus>,
    ) {
        let (should_restart, delay, config) = {
            let Ok(mut services) = self.services() else {
                return;
            };
            let Some(service) = services.get_mut(name) else {
                return;
            };
            if service.generation != generation || service.pid != pid {
                return;
            }
            let message = match &exit {
                Ok(status) => status.to_string(),
                Err(err) => format!("wait failed: {err}"),
            };
            let successful = exit.as_ref().is_ok_and(std::process::ExitStatus::success);
            service.pid = None;
            service.started_at = None;
            service.last_exit = Some(message);
            if !service.desired_running {
                service.state = ServiceState::Stopped;
                return;
            }
            let should_restart = service.should_restart(successful);
            if should_restart {
                service.state = ServiceState::BackingOff;
                let delay = service.advance_backoff();
                (true, delay, Some(service.config.clone()))
            } else {
                service.state = if successful {
                    ServiceState::Stopped
                } else {
                    ServiceState::Failed
                };
                (false, Duration::ZERO, None)
            }
        };

        if should_restart {
            time::sleep(delay).await;
            if let Some(config) = config {
                let desired = {
                    let Ok(services) = self.services() else {
                        return;
                    };
                    services
                        .get(&config.name)
                        .map(|service| service.desired_running)
                        .unwrap_or(false)
                };
                if desired {
                    if let Err(err) = self.spawn_and_track(config, true) {
                        tracing::error!("failed to restart {name}: {err}");
                    }
                }
            }
        }
    }

    async fn health_loop(&self, name: String, generation: u64) {
        loop {
            let config = {
                let Ok(services) = self.services() else {
                    return;
                };
                let Some(service) = services.get(&name) else {
                    return;
                };
                if !matches!(service.state, ServiceState::Running) {
                    return;
                }
                if service.generation != generation {
                    return;
                }
                service.config.healthcheck.clone()
            };
            let Some(config) = config else {
                return;
            };
            time::sleep(config.interval).await;
            let result = crate::health::check(&config).await;
            let mut restart = false;
            {
                let Ok(mut services) = self.services() else {
                    return;
                };
                let Some(service) = services.get_mut(&name) else {
                    return;
                };
                if !matches!(service.state, ServiceState::Running) {
                    return;
                }
                if service.generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        service.health_failures = 0;
                        service.health = HealthStatus::Healthy;
                    }
                    Err(err) => {
                        service.health_failures += 1;
                        let message = err.to_string();
                        if service.health_failures >= config.retries {
                            match config.action {
                                HealthAction::Ignore => {
                                    service.health = HealthStatus::Unhealthy(message);
                                }
                                HealthAction::MarkUnready => {
                                    service.health = HealthStatus::Unready(message);
                                }
                                HealthAction::Restart => {
                                    service.health = HealthStatus::Unhealthy(message);
                                    restart = true;
                                }
                            }
                        }
                    }
                }
            }
            if restart {
                let _ = self.restart(&name).await;
                return;
            }
        }
    }
}

async fn terminate_process_group(pid: u32, timeout: Duration) {
    let pgid = Pid::from_raw(pid as i32);
    let _ = killpg(pgid, Signal::SIGTERM);
    let deadline = time::Instant::now() + timeout;
    while time::Instant::now() < deadline {
        if !process_group_exists(pgid) {
            return;
        }
        time::sleep(Duration::from_millis(25)).await;
    }
    if process_group_exists(pgid) {
        let _ = killpg(pgid, Signal::SIGKILL);
    }
}

fn process_group_exists(pgid: Pid) -> bool {
    if unsafe { nix::libc::kill(-pgid.as_raw(), 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(nix::libc::ESRCH)
}
