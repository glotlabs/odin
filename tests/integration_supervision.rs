use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use odin::OdinError;
use odin::config::{
    self, ConfigSeverity, HealthAction, HealthCheckConfig, HealthCheckKind, RestartPolicy,
};
use odin::control::{ControlRequest, ControlResponse};
use odin::service::ServiceRuntime;
use odin::status::{
    MAX_EVENT_HISTORY, MAX_RESTART_HISTORY, RestartHistoryEntry, RestartReason, ServiceEventKind,
    ServiceState,
};
use odin::supervisor::SupervisorHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

fn temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_nanos();
    let dir = PathBuf::from("/tmp").join(format!("odin-{name}-{nonce}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_service(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(format!("{name}.toml"));
    fs::write(&path, body).expect("write service config");
    path
}

#[test]
fn invalid_toml_fails_clearly() {
    let dir = temp_dir("invalid");
    let path = write_service(&dir, "bad", "name = ");

    let err = config::load_service(&path).expect_err("invalid TOML must fail");
    assert!(err.to_string().contains("TOML parse error"));
}

#[test]
fn loads_service_defaults() {
    let dir = temp_dir("defaults");
    let path = write_service(
        &dir,
        "hello",
        r#"
name = "hello"
command = "/bin/sh"
args = ["-c", "echo hello"]
"#,
    );

    let service = config::load_service(&path).expect("service config should load");
    assert_eq!(service.name, "hello");
    assert_eq!(service.restart, RestartPolicy::Always);
    assert_eq!(service.restart_initial_delay, Duration::from_secs(1));
    assert_eq!(service.restart_max_delay, Duration::from_secs(30));
    assert!(service.autostart);
}

#[test]
fn duplicate_service_names_fail() {
    let dir = temp_dir("duplicate");
    write_service(
        &dir,
        "one",
        r#"
name = "same"
command = "/bin/sh"
"#,
    );
    write_service(
        &dir,
        "two",
        r#"
name = "same"
command = "/bin/sh"
"#,
    );

    let err = config::load_services(&dir).expect_err("duplicate names must fail");
    assert!(err.to_string().contains("duplicate service name"));
}

#[test]
fn duplicate_service_name_diagnostic_reports_both_files() {
    let dir = temp_dir("duplicate-diagnostic");
    let one = write_service(
        &dir,
        "one",
        r#"
name = "same"
command = "/bin/sh"
"#,
    );
    let two = write_service(
        &dir,
        "two",
        r#"
name = "same"
command = "/bin/sh"
"#,
    );

    let err = config::load_services(&dir).expect_err("duplicate names must fail");
    let OdinError::ConfigDiagnostics(diagnostics) = err else {
        panic!("expected config diagnostics");
    };

    let duplicate = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.field.as_deref() == Some("name"))
        .expect("duplicate diagnostic");
    assert_eq!(duplicate.severity, ConfigSeverity::Error);
    assert!(duplicate.message.contains(&one.display().to_string()));
    assert!(duplicate.message.contains(&two.display().to_string()));
    assert_eq!(
        duplicate.help.as_deref(),
        Some("Give each service a unique name across the config directory.")
    );
}

#[test]
fn validate_config_dir_collects_multiple_field_diagnostics() {
    let dir = temp_dir("validate-multiple");
    write_service(
        &dir,
        "bad",
        r#"
name = "bad"
command = "relative-command"
restart_initial_delay = "10s"
restart_max_delay = "1s"
stderr_log = "relative.log"

[healthcheck]
type = "tcp"
retries = 0
"#,
    );

    let err = config::validate_config_dir(&dir).expect_err("validation must fail");
    let OdinError::ConfigDiagnostics(diagnostics) = err else {
        panic!("expected config diagnostics");
    };
    let fields: Vec<_> = diagnostics
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigSeverity::Error)
        .filter_map(|diagnostic| diagnostic.field.as_deref())
        .collect();

    assert_eq!(diagnostics.service_count, 1);
    assert!(fields.contains(&"command"));
    assert!(fields.contains(&"restart_initial_delay"));
    assert!(fields.contains(&"stderr_log"));
    assert!(fields.contains(&"healthcheck.retries"));
    assert!(fields.contains(&"healthcheck.host"));
    assert!(fields.contains(&"healthcheck.port"));
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.path.ends_with("bad.toml"))
    );
}

#[test]
fn validate_config_dir_keeps_parse_location_and_other_file_errors() {
    let dir = temp_dir("validate-parse-location");
    let invalid = write_service(&dir, "invalid", "name = ");
    let missing = write_service(
        &dir,
        "missing",
        r#"
name = "missing"
command = "/definitely/not/a/odin/test/command"
"#,
    );

    let err = config::validate_config_dir(&dir).expect_err("validation must fail");
    let OdinError::ConfigDiagnostics(diagnostics) = err else {
        panic!("expected config diagnostics");
    };

    let parse = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.path == invalid)
        .expect("parse diagnostic");
    assert_eq!(parse.line, Some(1));
    assert!(parse.column.is_some());
    assert!(parse.message.contains("TOML parse error"));
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == missing
                && diagnostic.field.as_deref() == Some("command"))
    );
}

#[test]
fn validate_cli_json_reports_structured_errors() {
    let dir = temp_dir("validate-json");
    write_service(
        &dir,
        "bad",
        r#"
name = "bad"
command = "relative-command"
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_odin"))
        .arg("--config-dir")
        .arg(&dir)
        .arg("--json")
        .arg("validate")
        .output()
        .expect("run odin validate");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("validate JSON output");
    assert_eq!(payload["service_count"], 1);
    assert_eq!(payload["errors"][0]["severity"], "error");
    assert_eq!(payload["errors"][0]["field"], "command");
    assert!(
        payload["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .len()
            >= 1
    );
}

#[test]
fn add_service_file_derives_config_from_name() {
    let dir = temp_dir("add");
    let added = config::add_service_file(&dir, "my-app").expect("add service");

    assert_eq!(added.path, dir.join("my-app.toml"));
    assert_eq!(added.service.name, "my-app");
    assert_eq!(
        added.service.command,
        PathBuf::from("/usr/local/bin/my-app")
    );
    assert_eq!(added.service.cwd, Some(PathBuf::from("/usr/local/my-app")));
    assert_eq!(
        added.service.stdout_log,
        Some(PathBuf::from("/var/log/odin/my-app.out.log"))
    );
    assert_eq!(
        added.service.stderr_log,
        Some(PathBuf::from("/var/log/odin/my-app.err.log"))
    );
    assert!(added.service.autostart);
    assert_eq!(added.service.restart, RestartPolicy::Always);
    assert!(added.service.user.is_none());
    assert!(added.service.group.is_none());

    let loaded = config::load_service(&added.path).expect("generated TOML must load");
    assert_eq!(loaded, added.service);
}

#[test]
fn add_service_file_refuses_overwrite() {
    let dir = temp_dir("add-overwrite");
    config::add_service_file(&dir, "my-app").expect("first add");

    let err = config::add_service_file(&dir, "my-app").expect_err("overwrite must fail");
    assert!(err.to_string().contains("File exists") || err.to_string().contains("exists"));
}

#[test]
fn add_service_file_rejects_unsafe_name() {
    let dir = temp_dir("add-unsafe");

    let err = config::add_service_file(&dir, "../bad").expect_err("unsafe name must fail");
    assert!(err.to_string().contains("must not contain"));
}

#[test]
fn validate_config_dir_reports_valid_services_and_warnings() {
    let dir = temp_dir("validate");
    let log_dir = dir.join("missing-log-dir");
    write_service(
        &dir,
        "valid",
        &format!(
            r#"
name = "valid"
command = "/bin/sh"
autostart = false
stdout_log = "{}/valid.out.log"
"#,
            log_dir.display()
        ),
    );

    let report = config::validate_config_dir(&dir).expect("validation should pass");

    assert_eq!(report.service_count, 1);
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].message.contains("will be created"));
}

#[test]
fn validate_config_dir_rejects_missing_command() {
    let dir = temp_dir("validate-missing-command");
    write_service(
        &dir,
        "missing",
        r#"
name = "missing"
command = "/definitely/not/a/odin/test/command"
autostart = false
"#,
    );

    let err = config::validate_config_dir(&dir).expect_err("missing command must fail");
    assert!(err.to_string().contains("command does not exist"));
}

#[test]
fn restart_history_is_bounded() {
    let service = config::derive_service_config("bounded");
    let mut runtime = ServiceRuntime::new(service);

    for index in 0..(MAX_RESTART_HISTORY + 10) {
        runtime.record_restart(RestartHistoryEntry {
            at_unix_seconds: index as u64,
            reason: RestartReason::Automatic,
            from_pid: Some(index as u32),
            to_pid: Some((index + 1) as u32),
            exit: Some("exit status: 1".to_string()),
            backoff_millis: Some(100),
        });
    }

    let status = runtime.status();
    assert_eq!(status.restart_history.len(), MAX_RESTART_HISTORY);
    assert_eq!(status.restart_history[0].at_unix_seconds, 10);
}

#[test]
fn event_history_is_bounded() {
    let service = config::derive_service_config("events");
    let mut runtime = ServiceRuntime::new(service);

    for index in 0..(MAX_EVENT_HISTORY + 10) {
        runtime.record_event(ServiceEventKind::Started, format!("event {index}"));
    }

    let status = runtime.status();
    assert_eq!(status.event_history.len(), MAX_EVENT_HISTORY);
    assert_eq!(status.event_history[0].message, "event 10");
}

#[tokio::test]
async fn status_returns_configured_service_state() {
    let dir = temp_dir("status");
    let path = write_service(
        &dir,
        "idle",
        r#"
name = "idle"
command = "/bin/sh"
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let statuses = supervisor.status(Some("idle")).await.expect("status");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].name, "idle");
    assert_eq!(statuses[0].state, ServiceState::Stopped);
}

#[tokio::test]
async fn service_starts_successfully() {
    let dir = temp_dir("starts");
    let path = write_service(
        &dir,
        "sleeper",
        r#"
name = "sleeper"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    supervisor.start("sleeper").await.expect("start");
    let statuses = supervisor.status(Some("sleeper")).await.expect("status");
    assert_eq!(statuses[0].state, ServiceState::Running);
    assert!(statuses[0].pid.is_some());
    assert_eq!(statuses[0].restart_count, 0);
    assert!(
        statuses[0]
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::Started)
    );

    supervisor.stop("sleeper").await.expect("stop");
    let stopped = supervisor
        .status(Some("sleeper"))
        .await
        .expect("status after stop")
        .remove(0);
    assert!(
        stopped
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::StopRequested)
    );
    assert!(
        stopped
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::Stopped)
    );
}

#[tokio::test]
async fn restart_never_does_not_restart_after_exit() {
    let dir = temp_dir("never");
    let path = write_service(
        &dir,
        "oneshot",
        r#"
name = "oneshot"
command = "/bin/sh"
args = ["-c", "exit 7"]
autostart = false
restart = "never"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let err = supervisor
        .start("oneshot")
        .await
        .expect_err("start should report early exit");
    assert!(err.to_string().contains("failed startup"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let statuses = supervisor.status(Some("oneshot")).await.expect("status");

    assert_eq!(statuses[0].state, ServiceState::Failed);
    assert_eq!(statuses[0].pid, None);
    assert_eq!(statuses[0].restart_count, 0);
    assert!(statuses[0].last_exit.as_deref().unwrap_or("").contains("7"));
}

#[tokio::test]
async fn start_reports_failure_when_process_exits_during_startup_window() {
    let dir = temp_dir("startup-fail");
    let path = write_service(
        &dir,
        "bad-start",
        r#"
name = "bad-start"
command = "/bin/sh"
args = ["-c", "exit 42"]
autostart = false
restart = "never"
startup_timeout = "200ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let err = supervisor
        .start("bad-start")
        .await
        .expect_err("start should report early process exit");

    assert!(err.to_string().contains("failed startup"));
    let status = supervisor
        .status(Some("bad-start"))
        .await
        .expect("status")
        .remove(0);
    assert_eq!(status.state, ServiceState::Failed);
    assert!(status.last_exit.as_deref().unwrap_or("").contains("42"));
}

#[tokio::test]
async fn start_waits_for_healthcheck_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = listener.accept().await;
    });
    let dir = temp_dir("startup-health-ok");
    let path = write_service(
        &dir,
        "healthy-start",
        &format!(
            r#"
name = "healthy-start"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
startup_timeout = "1s"
stop_timeout = "100ms"

[healthcheck]
type = "tcp"
host = "127.0.0.1"
port = {port}
timeout = "100ms"
interval = "1s"
retries = 1
action = "ignore"
"#
        ),
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let result = supervisor
        .start("healthy-start")
        .await
        .expect("start should wait for healthy");

    assert_eq!(result.status.health, odin::status::HealthStatus::Healthy);
    assert_eq!(result.status.state, ServiceState::Running);
    server.await.expect("server task");
    supervisor.stop("healthy-start").await.expect("stop");
}

#[tokio::test]
async fn start_fails_when_healthcheck_does_not_pass() {
    let unused = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unused tcp");
    let port = unused.local_addr().expect("local addr").port();
    drop(unused);
    let dir = temp_dir("startup-health-fail");
    let path = write_service(
        &dir,
        "unhealthy-start",
        &format!(
            r#"
name = "unhealthy-start"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
startup_timeout = "150ms"
stop_timeout = "100ms"

[healthcheck]
type = "tcp"
host = "127.0.0.1"
port = {port}
timeout = "25ms"
interval = "1s"
retries = 1
action = "ignore"
"#
        ),
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let err = supervisor
        .start("unhealthy-start")
        .await
        .expect_err("start should fail when health never passes");

    assert!(err.to_string().contains("did not become healthy"));
    let status = supervisor
        .status(Some("unhealthy-start"))
        .await
        .expect("status")
        .remove(0);
    assert_eq!(status.state, ServiceState::Running);
    assert!(matches!(
        status.health,
        odin::status::HealthStatus::Unhealthy(_)
    ));
    assert!(
        status
            .event_history
            .iter()
            .any(|event| event.message.contains("startup health check failed"))
    );
    supervisor.stop("unhealthy-start").await.expect("stop");
}

#[tokio::test]
async fn service_stdio_without_logs_goes_to_dev_null() {
    let dir = temp_dir("stdio-null");
    let marker = dir.join("fds.txt");
    let path = write_service(
        &dir,
        "stdio",
        &format!(
            r#"
name = "stdio"
command = "/bin/sh"
args = ["-c", "exec 3>\"$MARKER\"; ls -l /dev/fd/0 /dev/fd/1 /dev/fd/2 >&3"]
autostart = false
restart = "never"
env = {{ MARKER = "{}" }}
"#,
            marker.display()
        ),
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let _ = supervisor.start("stdio").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let fds = fs::read_to_string(marker).expect("fd marker should exist");
    assert_eq!(fds.lines().count(), 3);
    for line in fds.lines() {
        assert_ne!(
            line.chars().next(),
            Some('p'),
            "managed service inherited a pipe fd: {line}"
        );
    }
}

#[tokio::test]
async fn service_stdout_and_stderr_are_logged_when_configured() {
    let dir = temp_dir("stdio-logs");
    let stdout_log = dir.join("service.out.log");
    let stderr_log = dir.join("service.err.log");
    let path = write_service(
        &dir,
        "logger",
        &format!(
            r#"
name = "logger"
command = "/bin/sh"
args = ["-c", "echo stdout-line; echo stderr-line >&2"]
autostart = false
restart = "never"
stdout_log = "{}"
stderr_log = "{}"
"#,
            stdout_log.display(),
            stderr_log.display()
        ),
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let _ = supervisor.start("logger").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        fs::read_to_string(stdout_log)
            .expect("stdout log")
            .contains("stdout-line")
    );
    assert!(
        fs::read_to_string(stderr_log)
            .expect("stderr log")
            .contains("stderr-line")
    );
}

#[tokio::test]
async fn crashing_service_restarts_with_backoff() {
    let dir = temp_dir("backoff");
    let path = write_service(
        &dir,
        "crasher",
        r#"
name = "crasher"
command = "/bin/sh"
args = ["-c", "exit 1"]
autostart = false
restart = "always"
restart_initial_delay = "50ms"
restart_max_delay = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    let err = supervisor
        .start("crasher")
        .await
        .expect_err("start should report crash loop");
    assert!(err.to_string().contains("failed startup"));
    tokio::time::sleep(Duration::from_millis(180)).await;
    let statuses = supervisor.status(Some("crasher")).await.expect("status");

    assert!(matches!(
        statuses[0].state,
        ServiceState::BackingOff | ServiceState::Running
    ));
    assert!(statuses[0].restart_count >= 1);
    let entry = statuses[0]
        .restart_history
        .last()
        .expect("automatic restart should be recorded");
    assert_eq!(entry.reason, RestartReason::Automatic);
    assert!(entry.exit.as_deref().unwrap_or("").contains("1"));
    assert_eq!(entry.backoff_millis, Some(50));
    assert!(entry.to_pid.is_some());
    assert!(
        statuses[0]
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::RestartScheduled)
    );
}

#[tokio::test]
async fn reload_adds_autostart_service() {
    let dir = temp_dir("reload-add");
    let path = write_service(
        &dir,
        "added",
        r#"
name = "added"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = true
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(Vec::new());

    let summary = supervisor.reload(vec![service]).await.expect("reload");
    assert_eq!(summary.added, vec!["added"]);
    let statuses = supervisor.status(Some("added")).await.expect("status");
    assert_eq!(statuses[0].state, ServiceState::Running);

    supervisor.stop("added").await.expect("stop");
}

#[tokio::test]
async fn reload_removes_running_service() {
    let dir = temp_dir("reload-remove");
    let path = write_service(
        &dir,
        "gone",
        r#"
name = "gone"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    supervisor.start("gone").await.expect("start");
    let summary = supervisor.reload(Vec::new()).await.expect("reload");

    assert_eq!(summary.removed, vec!["gone"]);
    assert!(supervisor.status(Some("gone")).await.is_err());
}

#[tokio::test]
async fn reload_live_update_keeps_running_process() {
    let dir = temp_dir("reload-live");
    let original = write_service(
        &dir,
        "live",
        r#"
name = "live"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let updated = write_service(
        &dir,
        "live-updated",
        r#"
name = "live"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "always"
restart_initial_delay = "20ms"
restart_max_delay = "20ms"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&original).expect("service config should load");
    let updated_service = config::load_service(&updated).expect("updated config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    supervisor.start("live").await.expect("start");
    let before = supervisor.status(Some("live")).await.expect("status")[0]
        .pid
        .expect("pid before");
    let summary = supervisor
        .reload(vec![updated_service])
        .await
        .expect("reload");
    let after = supervisor.status(Some("live")).await.expect("status")[0]
        .pid
        .expect("pid after");

    assert_eq!(summary.live_updated, vec!["live"]);
    assert!(summary.restarted.is_empty());
    assert_eq!(before, after);
    let status = supervisor
        .status(Some("live"))
        .await
        .expect("status after reload")
        .remove(0);
    assert!(
        status
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::ReloadUpdated)
    );

    supervisor.stop("live").await.expect("stop");
}

#[tokio::test]
async fn reload_process_change_restarts_running_process() {
    let dir = temp_dir("reload-restart");
    let original = write_service(
        &dir,
        "worker",
        r#"
name = "worker"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let updated = write_service(
        &dir,
        "worker-updated",
        r#"
name = "worker"
command = "/bin/sh"
args = ["-c", "sleep 61"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&original).expect("service config should load");
    let updated_service = config::load_service(&updated).expect("updated config should load");
    let supervisor = SupervisorHandle::new(vec![service]);

    supervisor.start("worker").await.expect("start");
    let before = supervisor.status(Some("worker")).await.expect("status")[0]
        .pid
        .expect("pid before");
    let summary = supervisor
        .reload(vec![updated_service])
        .await
        .expect("reload");
    let after = supervisor.status(Some("worker")).await.expect("status")[0]
        .pid
        .expect("pid after");

    assert_eq!(summary.restarted, vec!["worker"]);
    assert!(summary.live_updated.is_empty());
    assert_ne!(before, after);
    let status = supervisor
        .status(Some("worker"))
        .await
        .expect("status after reload restart")
        .remove(0);
    assert!(
        status
            .event_history
            .iter()
            .any(|event| event.kind == ServiceEventKind::ReloadRestartRequired)
    );

    supervisor.stop("worker").await.expect("stop");
}

#[tokio::test]
async fn command_health_check_success_and_timeout() {
    let ok = HealthCheckConfig {
        kind: HealthCheckKind::Command,
        command: Some("/bin/sh".into()),
        args: vec!["-c".to_string(), "exit 0".to_string()],
        host: None,
        port: None,
        url: None,
        interval: Duration::from_secs(1),
        startup_grace: Duration::ZERO,
        timeout: Duration::from_secs(1),
        retries: 1,
        action: HealthAction::Restart,
    };
    odin::health::check(&ok).await.expect("healthy command");

    let timeout = HealthCheckConfig {
        args: vec!["-c".to_string(), "sleep 5".to_string()],
        timeout: Duration::from_millis(50),
        ..ok
    };
    let err = odin::health::check(&timeout)
        .await
        .expect_err("timeout should fail");
    assert!(err.to_string().contains("timed out"));
}

#[tokio::test]
async fn tcp_health_check_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = listener.accept().await;
    });
    let check = HealthCheckConfig {
        kind: HealthCheckKind::Tcp,
        command: None,
        args: Vec::new(),
        host: Some("127.0.0.1".to_string()),
        port: Some(port),
        url: None,
        interval: Duration::from_secs(1),
        startup_grace: Duration::ZERO,
        timeout: Duration::from_secs(1),
        retries: 1,
        action: HealthAction::Restart,
    };

    odin::health::check(&check).await.expect("tcp healthy");
    server.await.expect("server task");
}

#[tokio::test]
async fn http_health_check_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
    let url = format!(
        "http://{}/health",
        listener.local_addr().expect("local addr")
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("write response");
    });
    let check = HealthCheckConfig {
        kind: HealthCheckKind::Http,
        command: None,
        args: Vec::new(),
        host: None,
        port: None,
        url: Some(url),
        interval: Duration::from_secs(1),
        startup_grace: Duration::ZERO,
        timeout: Duration::from_secs(1),
        retries: 1,
        action: HealthAction::Restart,
    };

    odin::health::check(&check).await.expect("http healthy");
    server.await.expect("server task");
}

#[tokio::test]
async fn control_status_round_trips_over_unix_socket() {
    let dir = temp_dir("control");
    let socket = dir.join("odin.sock");
    let path = write_service(
        &dir,
        "idle",
        r#"
name = "idle"
command = "/bin/sh"
autostart = false
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);
    let server_supervisor = supervisor.clone();
    let server_socket = socket.clone();
    let server_config_dir = dir.clone();

    let server = tokio::spawn(async move {
        let _ = odin::control::serve(&server_socket, server_config_dir, server_supervisor).await;
    });

    for _ in 0..100 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = odin::control::request(&socket, ControlRequest::Status { service: None })
        .await
        .expect("control request should succeed");
    server.abort();

    match response {
        ControlResponse::Status { services } => {
            assert_eq!(services.len(), 1);
            assert_eq!(services[0].name, "idle");
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn control_reload_loads_new_service_from_config_dir() {
    let dir = temp_dir("control-reload");
    let socket = dir.join("odin.sock");
    let supervisor = SupervisorHandle::new(Vec::new());
    let server_supervisor = supervisor.clone();
    let server_socket = socket.clone();
    let server_config_dir = dir.clone();

    let server = tokio::spawn(async move {
        let _ = odin::control::serve(&server_socket, server_config_dir, server_supervisor).await;
    });

    for _ in 0..100 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    write_service(
        &dir,
        "added",
        r#"
name = "added"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );

    let response = odin::control::request(&socket, ControlRequest::Reload)
        .await
        .expect("reload request should succeed");

    match response {
        ControlResponse::Reload { summary } => {
            assert_eq!(summary.added, vec!["added"]);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert_eq!(
        supervisor.status(Some("added")).await.expect("status")[0].name,
        "added"
    );

    server.abort();
}

#[tokio::test]
async fn control_start_failure_includes_status_feedback() {
    let dir = temp_dir("control-start-failure");
    let socket = dir.join("odin.sock");
    let path = write_service(
        &dir,
        "bad-start",
        r#"
name = "bad-start"
command = "/bin/sh"
args = ["-c", "exit 42"]
autostart = false
restart = "never"
startup_timeout = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);
    let server_supervisor = supervisor.clone();
    let server_socket = socket.clone();
    let server_config_dir = dir.clone();
    let server = tokio::spawn(async move {
        let _ = odin::control::serve(&server_socket, server_config_dir, server_supervisor).await;
    });

    for _ in 0..100 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = odin::control::request(
        &socket,
        ControlRequest::Start {
            service: "bad-start".to_string(),
        },
    )
    .await
    .expect("start request should receive response");

    match response {
        ControlResponse::Error { error } => {
            assert_eq!(error.code, "protocol");
            let status = error.status.expect("operation error should include status");
            assert_eq!(status.state, ServiceState::Failed);
            assert!(status.last_exit.as_deref().unwrap_or("").contains("42"));
            assert!(
                status
                    .event_history
                    .iter()
                    .any(|event| event.kind == ServiceEventKind::Exited)
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn control_restart_restarts_service() {
    let dir = temp_dir("control-restart");
    let socket = dir.join("odin.sock");
    let path = write_service(
        &dir,
        "worker",
        r#"
name = "worker"
command = "/bin/sh"
args = ["-c", "sleep 60"]
autostart = false
restart = "never"
stop_timeout = "100ms"
"#,
    );
    let service = config::load_service(&path).expect("service config should load");
    let supervisor = SupervisorHandle::new(vec![service]);
    supervisor.start("worker").await.expect("start");
    let before = supervisor
        .status(Some("worker"))
        .await
        .expect("status before")[0]
        .pid
        .expect("pid before");

    let server_supervisor = supervisor.clone();
    let server_socket = socket.clone();
    let server_config_dir = dir.clone();
    let server = tokio::spawn(async move {
        let _ = odin::control::serve(&server_socket, server_config_dir, server_supervisor).await;
    });

    for _ in 0..100 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = odin::control::request(
        &socket,
        ControlRequest::Restart {
            service: "worker".to_string(),
        },
    )
    .await
    .expect("restart request should succeed");
    let restarted = match response {
        ControlResponse::Operation { result } => result,
        other => panic!("unexpected response: {other:?}"),
    };

    let after = supervisor
        .status(Some("worker"))
        .await
        .expect("status after")[0]
        .pid
        .expect("pid after");
    assert_ne!(before, after);
    assert_eq!(restarted.status.pid, Some(after));
    let status = supervisor
        .status(Some("worker"))
        .await
        .expect("status after restart")
        .remove(0);
    let entry = status
        .restart_history
        .last()
        .expect("manual restart should be recorded");
    assert_eq!(entry.reason, RestartReason::Manual);
    assert_eq!(entry.from_pid, Some(before));
    assert_eq!(entry.to_pid, Some(after));

    server.abort();
    supervisor.stop("worker").await.expect("stop");
}
