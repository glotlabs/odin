use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::sync::oneshot;
use tokio::time;

use crate::child;
use crate::config::{HealthAction, ServiceConfig};
use crate::error::{OdinError, Result};
use crate::service::ServiceRuntime;
use crate::status::{
    HealthStatus, RestartHistoryEntry, RestartReason, ServiceEventKind, ServiceState,
    ServiceStatus, now_unix_seconds,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReloadSummary {
    pub added: Vec<String>,
    pub live_updated: Vec<String>,
    pub restarted: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OperationResult {
    pub service: String,
    pub action: OperationAction,
    pub message: String,
    pub status: ServiceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationAction {
    Started,
    Stopped,
    Restarted,
}

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
            .map_err(|_| OdinError::Protocol("supervisor state lock poisoned".to_string()))
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
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            return Ok(vec![service.status()]);
        }
        Ok(services.values().map(ServiceRuntime::status).collect())
    }

    pub async fn start(&self, name: &str) -> Result<OperationResult> {
        let config = {
            let mut services = self.services()?;
            let service = services
                .get_mut(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            if matches!(
                service.state,
                ServiceState::Running | ServiceState::Starting | ServiceState::Stopping
            ) {
                return Err(OdinError::AlreadyRunning(name.to_string()));
            }
            service.desired_running = true;
            service.state = ServiceState::Starting;
            service.record_event(ServiceEventKind::Started, "start requested");
            service.config.clone()
        };
        self.spawn_and_track(config.clone(), None)?;
        self.wait_for_startup(&config.name, config.startup_timeout)
            .await?;
        self.operation_result(&config.name, OperationAction::Started, "service started")
    }

    pub async fn stop(&self, name: &str) -> Result<OperationResult> {
        let (pid, timeout, stopped) = {
            let mut services = self.services()?;
            let service = services
                .get_mut(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            service.desired_running = false;
            if service.pid.is_none() {
                service.state = ServiceState::Stopped;
                service.record_event(ServiceEventKind::Stopped, "service already stopped");
                service.notify_stopped();
                return self.operation_result(
                    name,
                    OperationAction::Stopped,
                    "service already stopped",
                );
            }
            service.state = ServiceState::Stopping;
            service.record_event(
                ServiceEventKind::StopRequested,
                format!("stop requested for pid {}", service.pid.unwrap_or_default()),
            );
            let (tx, rx) = oneshot::channel();
            service.stop_waiters.push(tx);
            (service.pid, service.config.stop_timeout, rx)
        };

        let Some(pid) = pid else {
            return self.operation_result(
                name,
                OperationAction::Stopped,
                "service already stopped",
            );
        };
        let escalated = terminate_process_group(pid, timeout, stopped).await;
        if escalated {
            let mut services = self.services()?;
            if let Some(service) = services.get_mut(name) {
                service.record_event(
                    ServiceEventKind::StopRequested,
                    format!("stop escalated to SIGKILL for pid {pid}"),
                );
            }
        }
        self.operation_result(name, OperationAction::Stopped, "service stopped")
    }

    pub async fn restart(&self, name: &str) -> Result<OperationResult> {
        self.restart_with_reason(name, RestartReason::Manual).await
    }

    async fn restart_with_reason(
        &self,
        name: &str,
        reason: RestartReason,
    ) -> Result<OperationResult> {
        let from_pid = {
            let services = self.services()?;
            let service = services
                .get(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            service.pid
        };
        let exists_and_running = {
            let services = self.services()?;
            let service = services
                .get(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            service.pid.is_some()
        };
        if exists_and_running {
            self.stop(name).await?;
        }
        let config = {
            let mut services = self.services()?;
            let service = services
                .get_mut(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
            if matches!(
                service.state,
                ServiceState::Running | ServiceState::Starting | ServiceState::Stopping
            ) {
                return Err(OdinError::AlreadyRunning(name.to_string()));
            }
            service.desired_running = true;
            service.state = ServiceState::Starting;
            service.config.clone()
        };
        self.spawn_and_track(
            config,
            Some(RestartHistoryEntry {
                at_unix_seconds: now_unix_seconds(),
                reason,
                from_pid,
                to_pid: None,
                exit: None,
                backoff_millis: None,
            }),
        )?;
        let config = {
            let services = self.services()?;
            services
                .get(name)
                .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?
                .config
                .clone()
        };
        self.wait_for_startup(name, config.startup_timeout).await?;
        self.operation_result(name, OperationAction::Restarted, "service restarted")
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

    pub async fn reload(&self, new_configs: Vec<ServiceConfig>) -> Result<ReloadSummary> {
        let new_by_name = new_configs
            .into_iter()
            .map(|config| (config.name.clone(), config))
            .collect::<BTreeMap<_, _>>();
        let (removed_running, restart_names, start_required, health_loop_required, summary) = {
            let mut services = self.services()?;
            let mut summary = ReloadSummary::default();
            let mut restart_names = Vec::new();
            let mut start_required = Vec::new();
            let mut health_loop_required = Vec::new();

            for (name, config) in &new_by_name {
                match services.get_mut(name) {
                    Some(service) if service.config != *config => {
                        let was_running = service.pid.is_some();
                        let needs_restart =
                            was_running && restart_required(&service.config, config);
                        let needs_health_loop = was_running
                            && service.config.healthcheck.is_none()
                            && config.healthcheck.is_some();
                        service.config = config.clone();
                        service.desired_running = config.autostart || was_running;
                        service.reset_backoff();
                        if needs_restart {
                            service.record_event(
                                ServiceEventKind::ReloadRestartRequired,
                                "reload changed process-affecting configuration",
                            );
                            summary.restarted.push(name.clone());
                            restart_names.push(name.clone());
                        } else {
                            service.record_event(
                                ServiceEventKind::ReloadUpdated,
                                "reload applied live configuration update",
                            );
                            summary.live_updated.push(name.clone());
                            if needs_health_loop {
                                health_loop_required.push((name.clone(), service.generation));
                            }
                            if !was_running && config.autostart {
                                start_required.push(name.clone());
                            }
                        }
                    }
                    Some(service) if service.pid.is_none() && service.config.autostart => {
                        start_required.push(name.clone());
                    }
                    Some(_) => {}
                    None => {
                        let mut runtime = ServiceRuntime::new(config.clone());
                        runtime.record_event(ServiceEventKind::Added, "service added by reload");
                        services.insert(name.clone(), runtime);
                        summary.added.push(name.clone());
                        if config.autostart {
                            start_required.push(name.clone());
                        }
                    }
                }
            }

            let existing_names = services.keys().cloned().collect::<Vec<_>>();
            let mut removed_running = Vec::new();
            for name in existing_names {
                if new_by_name.contains_key(&name) {
                    continue;
                }
                let Some(service) = services.get_mut(&name) else {
                    continue;
                };
                service.desired_running = false;
                service.record_event(ServiceEventKind::Removed, "service removed by reload");
                summary.removed.push(name.clone());
                if service.pid.is_some() {
                    removed_running.push(name);
                } else {
                    services.remove(&name);
                }
            }

            (
                removed_running,
                restart_names,
                start_required,
                health_loop_required,
                summary,
            )
        };

        for name in removed_running {
            let _ = self.stop(&name).await;
            let mut services = self.services()?;
            services.remove(&name);
        }

        for name in restart_names {
            self.restart_with_reason(&name, RestartReason::Reload)
                .await?;
        }

        for name in start_required {
            let should_start = {
                let services = self.services()?;
                services
                    .get(&name)
                    .map(|service| service.pid.is_none() && service.config.autostart)
                    .unwrap_or(false)
            };
            if should_start {
                self.start(&name).await?;
            }
        }

        for (name, generation) in health_loop_required {
            self.spawn_health_loop(name, generation);
        }

        Ok(summary)
    }

    fn spawn_and_track(
        &self,
        config: ServiceConfig,
        restart_entry: Option<RestartHistoryEntry>,
    ) -> Result<()> {
        let mut child = child::spawn_service(&config)?;
        let pid = child.id();
        let generation;
        {
            let mut services = self.services()?;
            let service = services
                .get_mut(&config.name)
                .ok_or_else(|| OdinError::ServiceNotFound(config.name.clone()))?;
            service.state = ServiceState::Running;
            service.pid = pid;
            service.generation += 1;
            generation = service.generation;
            service.started_at = Some(std::time::Instant::now());
            service.last_exit = None;
            service.health = HealthStatus::Unknown;
            service.health_failures = 0;
            service.reset_backoff();
            if let Some(mut entry) = restart_entry {
                entry.to_pid = pid;
                service.restart_count += 1;
                service.record_restart(entry);
                service.record_event(
                    ServiceEventKind::Restarted,
                    format!("service restarted with pid {}", pid.unwrap_or_default()),
                );
            } else {
                service.record_event(
                    ServiceEventKind::Started,
                    format!("service started with pid {}", pid.unwrap_or_default()),
                );
            }
        }

        let handle = self.clone();
        let name = config.name.clone();
        tokio::spawn(async move {
            let exit = child.wait().await;
            handle.handle_exit(&name, generation, pid, exit).await;
        });

        if config.healthcheck.is_some() {
            self.spawn_health_loop(config.name, generation);
        }

        Ok(())
    }

    async fn wait_for_startup(&self, name: &str, timeout: Duration) -> Result<()> {
        let deadline = time::Instant::now() + timeout;
        let mut startup_grace_applied = false;
        let mut last_health_error = None;
        loop {
            let (status, healthcheck) = {
                let services = self.services()?;
                let service = services
                    .get(name)
                    .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
                (service.status(), service.config.healthcheck.clone())
            };

            match status.state {
                ServiceState::Failed | ServiceState::Stopped | ServiceState::BackingOff => {
                    return Err(OdinError::Protocol(format!(
                        "service {name} failed startup: state={:?}, last_exit={}",
                        status.state,
                        status.last_exit.unwrap_or_else(|| "unknown".to_string())
                    )));
                }
                ServiceState::Starting | ServiceState::Stopping => {}
                ServiceState::Running => {}
            }

            let has_healthcheck = healthcheck.is_some();
            if let Some(mut healthcheck) = healthcheck {
                if !startup_grace_applied {
                    startup_grace_applied = true;
                    if !healthcheck.startup_grace.is_zero() {
                        let remaining = deadline.saturating_duration_since(time::Instant::now());
                        if remaining.is_zero() {
                            return Err(OdinError::Protocol(format!(
                                "service {name} did not become healthy within {}ms; health={:?}",
                                timeout.as_millis(),
                                status.health
                            )));
                        }
                        time::sleep(healthcheck.startup_grace.min(remaining)).await;
                        continue;
                    }
                }

                let remaining = deadline.saturating_duration_since(time::Instant::now());
                if remaining.is_zero() {
                    return Err(OdinError::Protocol(format!(
                        "service {name} did not become healthy within {}ms; health={:?}",
                        timeout.as_millis(),
                        status.health
                    )));
                }
                healthcheck.timeout = healthcheck.timeout.min(remaining);

                match crate::health::check(&healthcheck).await {
                    Ok(()) => {
                        let mut services = self.services()?;
                        if let Some(service) = services.get_mut(name) {
                            service.health_failures = 0;
                            if !matches!(service.health, HealthStatus::Healthy) {
                                service.record_event(
                                    ServiceEventKind::HealthChanged,
                                    "startup health check passed",
                                );
                            }
                            service.health = HealthStatus::Healthy;
                        }
                        return Ok(());
                    }
                    Err(err) => {
                        let message = err.to_string();
                        last_health_error = Some(message.clone());
                        let mut services = self.services()?;
                        if let Some(service) = services.get_mut(name) {
                            service.health_failures += 1;
                            service.health = HealthStatus::Unhealthy(message.clone());
                            service.record_event(
                                ServiceEventKind::HealthChanged,
                                format!("startup health check failed: {message}"),
                            );
                        }
                    }
                }
            }

            if time::Instant::now() >= deadline {
                if !has_healthcheck && status.state == ServiceState::Running {
                    return Ok(());
                }
                if let Some(message) = last_health_error {
                    return Err(OdinError::Protocol(format!(
                        "service {name} did not become healthy within {}ms: {message}",
                        timeout.as_millis()
                    )));
                }
                return Err(OdinError::Protocol(format!(
                    "service {name} did not stay running within {}ms; state={:?}",
                    timeout.as_millis(),
                    status.state
                )));
            }
            time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn operation_result(
        &self,
        name: &str,
        action: OperationAction,
        message: impl Into<String>,
    ) -> Result<OperationResult> {
        let status = self
            .status_now(name)?
            .ok_or_else(|| OdinError::ServiceNotFound(name.to_string()))?;
        Ok(OperationResult {
            service: name.to_string(),
            action,
            message: message.into(),
            status,
        })
    }

    fn status_now(&self, name: &str) -> Result<Option<ServiceStatus>> {
        let services = self.services()?;
        Ok(services.get(name).map(ServiceRuntime::status))
    }

    fn spawn_health_loop(&self, name: String, generation: u64) {
        let handle = self.clone();
        tokio::spawn(async move {
            handle.health_loop(name, generation).await;
        });
    }

    async fn handle_exit(
        &self,
        name: &str,
        generation: u64,
        pid: Option<u32>,
        exit: std::io::Result<std::process::ExitStatus>,
    ) {
        let (should_restart, delay, config, exit_message) = {
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
            service.last_exit = Some(message.clone());
            service.record_event(ServiceEventKind::Exited, message.clone());
            service.notify_stopped();
            if !service.desired_running {
                service.state = ServiceState::Stopped;
                service.record_event(ServiceEventKind::Stopped, "service stopped");
                return;
            }
            let should_restart = service.should_restart(successful);
            if should_restart {
                service.state = ServiceState::BackingOff;
                let delay = service.advance_backoff();
                service.record_event(
                    ServiceEventKind::RestartScheduled,
                    format!("restart scheduled after {}ms", delay.as_millis()),
                );
                (true, delay, Some(service.config.clone()), message)
            } else {
                service.state = if successful {
                    ServiceState::Stopped
                } else {
                    ServiceState::Failed
                };
                (false, Duration::ZERO, None, message)
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
                if desired
                    && let Err(err) = self.spawn_and_track(
                        config,
                        Some(RestartHistoryEntry {
                            at_unix_seconds: now_unix_seconds(),
                            reason: RestartReason::Automatic,
                            from_pid: pid,
                            to_pid: None,
                            exit: Some(exit_message),
                            backoff_millis: Some(delay.as_millis() as u64),
                        }),
                    )
                {
                    tracing::error!("failed to restart {name}: {err}");
                }
            }
        }
    }

    async fn health_loop(&self, name: String, generation: u64) {
        let mut first_check = true;
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
            if first_check && !config.startup_grace.is_zero() {
                time::sleep(config.startup_grace).await;
            }
            first_check = false;
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
                        if !matches!(service.health, HealthStatus::Healthy) {
                            service.record_event(
                                ServiceEventKind::HealthChanged,
                                "health changed to healthy",
                            );
                        }
                        service.health = HealthStatus::Healthy;
                    }
                    Err(err) => {
                        service.health_failures += 1;
                        let message = err.to_string();
                        if service.health_failures >= config.retries {
                            match config.action {
                                HealthAction::Ignore => {
                                    service.record_event(
                                        ServiceEventKind::HealthChanged,
                                        format!("health changed to unhealthy: {message}"),
                                    );
                                    service.health = HealthStatus::Unhealthy(message);
                                }
                                HealthAction::MarkUnready => {
                                    service.record_event(
                                        ServiceEventKind::HealthChanged,
                                        format!("health changed to unready: {message}"),
                                    );
                                    service.health = HealthStatus::Unready(message);
                                }
                                HealthAction::Restart => {
                                    service.record_event(
                                        ServiceEventKind::HealthChanged,
                                        format!("health changed to unhealthy: {message}"),
                                    );
                                    service.health = HealthStatus::Unhealthy(message);
                                    restart = true;
                                }
                            }
                        }
                    }
                }
            }
            if restart {
                let _ = self
                    .restart_with_reason(&name, RestartReason::HealthCheck)
                    .await;
                return;
            }
        }
    }
}

async fn terminate_process_group(
    pid: u32,
    timeout: Duration,
    stopped: oneshot::Receiver<()>,
) -> bool {
    let pgid = Pid::from_raw(pid as i32);
    let _ = killpg(pgid, Signal::SIGTERM);
    if time::timeout(timeout, stopped).await.is_ok() {
        return false;
    }
    if process_group_exists(pgid) {
        let _ = killpg(pgid, Signal::SIGKILL);
        return true;
    }
    false
}

fn process_group_exists(pgid: Pid) -> bool {
    if unsafe { nix::libc::kill(-pgid.as_raw(), 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(nix::libc::ESRCH)
}

fn restart_required(old: &ServiceConfig, new: &ServiceConfig) -> bool {
    old.command != new.command
        || old.args != new.args
        || old.cwd != new.cwd
        || old.env != new.env
        || old.user != new.user
        || old.group != new.group
        || old.umask != new.umask
        || old.stdout_log != new.stdout_log
        || old.stderr_log != new.stderr_log
}
