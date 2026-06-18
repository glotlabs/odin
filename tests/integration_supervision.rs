use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use supper::config::{self, RestartPolicy};
use supper::control::{ControlRequest, ControlResponse};
use supper::status::ServiceState;
use supper::supervisor::SupervisorHandle;

fn temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("supper-{name}-{nonce}"));
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

    supervisor.stop("sleeper").await.expect("stop");
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

    supervisor.start("oneshot").await.expect("start");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let statuses = supervisor.status(Some("oneshot")).await.expect("status");

    assert_eq!(statuses[0].state, ServiceState::Failed);
    assert_eq!(statuses[0].pid, None);
    assert_eq!(statuses[0].restart_count, 0);
    assert!(statuses[0].last_exit.as_deref().unwrap_or("").contains("7"));
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

    supervisor.start("crasher").await.expect("start");
    tokio::time::sleep(Duration::from_millis(180)).await;
    let statuses = supervisor.status(Some("crasher")).await.expect("status");

    assert!(matches!(
        statuses[0].state,
        ServiceState::BackingOff | ServiceState::Running
    ));
    assert!(statuses[0].restart_count >= 1);
}

#[tokio::test]
async fn control_status_round_trips_over_unix_socket() {
    let dir = temp_dir("control");
    let socket = dir.join("supper.sock");
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

    let server = tokio::spawn(async move {
        let _ = supper::control::serve(&server_socket, server_supervisor).await;
    });

    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = supper::control::request(&socket, ControlRequest::Status { service: None })
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
