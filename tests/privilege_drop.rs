use std::path::PathBuf;
use std::time::Duration;

use supper::config::{RestartPolicy, ServiceConfig};
use supper::privileges::Privileges;

fn service_with_user(user: &str, group: Option<&str>) -> ServiceConfig {
    ServiceConfig {
        name: "privilege-test".to_string(),
        command: PathBuf::from("/bin/sh"),
        args: Vec::new(),
        cwd: None,
        autostart: false,
        env: Default::default(),
        user: Some(user.to_string()),
        group: group.map(str::to_string),
        umask: None,
        restart: RestartPolicy::Never,
        restart_initial_delay: Duration::from_secs(1),
        restart_max_delay: Duration::from_secs(1),
        stop_timeout: Duration::from_secs(1),
        stdout_log: None,
        stderr_log: None,
        healthcheck: None,
    }
}

#[test]
#[ignore = "root-only: drops the current test process privileges"]
fn drops_to_nobody() {
    let privileges =
        Privileges::resolve(&service_with_user("nobody", None)).expect("resolve nobody");
    privileges.apply().expect("drop privileges");
    assert_eq!(nix::unistd::Uid::current().as_raw(), 65534);
}

#[test]
fn unknown_user_fails() {
    let err = Privileges::resolve(&service_with_user(
        "supper-user-that-should-not-exist",
        None,
    ))
    .expect_err("unknown user must fail");
    assert!(err.to_string().contains("unknown user"));
}
