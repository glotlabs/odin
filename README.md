# odin

`odin` is a minimal Rust service supervisor for FreeBSD-hosted application processes.
It is intended for long-running third-party apps you own, not for replacing `init`,
`rc.d`, or system service management.

## Layout

- Service configs: `/opt/odin/etc/odin/services/*.toml`
- Control socket: `/var/run/odin.sock`
- Service logs: `/var/log/odin`
- Future persistent state, if needed: `/var/db/odin`

## Commands

Run the foreground supervisor:

```sh
odin --config-dir /opt/odin/etc/odin/services --socket /var/run/odin.sock serve
```

Query and control services:

```sh
odin status
odin status my-app
odin --json status
odin events my-app
odin --json events my-app
odin add my-app
odin validate
odin --json validate
odin reload
odin --json reload
odin start my-app
odin stop my-app
odin restart my-app
```

`odin status` includes current uptime and the most recent restart reason in the
human table.
`odin --json status` includes bounded restart history for each service. The
history keeps the last 64 restart records with timestamp, reason, previous PID,
new PID, exit text when available, and backoff delay in milliseconds.

`odin --json status` also includes bounded event history for each retained service. The
event history keeps the last 128 lifecycle records, including starts, exits,
stops, scheduled restarts, reload updates, reload-required restarts, and health
state changes. Reload removals are reported in the reload summary; once a
service is removed, its per-service history is no longer queryable. Use
`odin events <service>` for a compact human view of one service's event history.

## Service Config

```toml
name = "my-app"
command = "/opt/odin/bin/my-app"
args = ["--port", "8080"]
cwd = "/opt/odin/my-app"
autostart = true

env = { RUST_LOG = "info" }

user = "myapp"
group = "myapp"

restart = "always"
restart_initial_delay = "1s"
restart_max_delay = "30s"
startup_timeout = "2s"

stdout_log = "/var/log/odin/my-app.out.log"
stderr_log = "/var/log/odin/my-app.err.log"

[healthcheck]
type = "tcp"
host = "127.0.0.1"
port = 8080
interval = "10s"
timeout = "2s"
retries = 3
action = "restart"
```

Restart policies are `never`, `on-failure`, and `always`.
Health check types are `command`, `tcp`, and `http`.
Health actions are `ignore`, `mark-unready`, and `restart`.
TCP health check `host` accepts an IP address or DNS name.

`startup_timeout` controls start/restart acknowledgement. `odin start` and
`odin restart` return success only after the process remains running through
that startup window. If it exits early, the command fails and includes current
status, last exit, and recent events so you do not need to hunt through logs for
basic startup failures.

When a service has a health check, startup acknowledgement is stricter:
`odin start` and `odin restart` return success only after the health check
passes within `startup_timeout`. `healthcheck.startup_grace` is respected before
the first startup health probe. If the process stays alive but never becomes
healthy, the command fails with health status and recent health events.

`odin serve` handles `SIGHUP` by reloading the config directory. Reloading:

- adds new services and starts them when `autostart = true`
- applies live-only changes without restarting the process
- restarts running services when process-affecting fields change
- stops and removes services deleted from the config directory
- keeps already-running services running when their config still exists

You can also trigger the same reload over the control socket:

```sh
odin reload
```

That is the preferred form for deploy scripts and CI jobs because it does not
require finding the supervisor PID or using rc.d-specific commands.

Process-affecting fields are `command`, `args`, `cwd`, `env`, `user`, `group`,
`umask`, `stdout_log`, and `stderr_log`. Restart policy, restart delays, stop
timeout, autostart, and health-check changes are live updates.

`odin validate` checks all service config files without starting anything. It
parses TOML, checks duplicate names, validates user/group lookups, verifies
absolute command paths exist, checks `cwd`, and reports log directories that
will be created by the supervisor.

Config errors are reported with the config file, field, source line, caret, and
a short help message when the location is known:

```text
error: /opt/odin/etc/odin/services/web.toml:3:11 command: command must be an absolute path
     3 | command = "web"
       |           ^
  help: Set command to an absolute path such as "/opt/odin/bin/app".
```

`odin validate` keeps checking after the first problem and reports all config
diagnostics it can collect. `odin --json validate` returns the same information
as structured `errors`, `warnings`, and `diagnostics` arrays:

```json
{
  "service_count": 1,
  "errors": [
    {
      "severity": "error",
      "path": "/opt/odin/etc/odin/services/web.toml",
      "line": 3,
      "column": 11,
      "service": "web",
      "field": "command",
      "message": "command must be an absolute path",
      "help": "Set command to an absolute path such as \"/opt/odin/bin/app\"."
    }
  ],
  "warnings": [],
  "diagnostics": [
    {
      "severity": "error",
      "path": "/opt/odin/etc/odin/services/web.toml",
      "line": 3,
      "column": 11,
      "service": "web",
      "field": "command",
      "message": "command must be an absolute path",
      "help": "Set command to an absolute path such as \"/opt/odin/bin/app\"."
    }
  ]
}
```

Reload uses the same diagnostics. If `odin reload` asks the supervisor to reload an
invalid config directory, the command prints source/caret diagnostics and exits
non-zero. `odin --json reload` returns a structured control error with
`code = "invalid-config"` and a `config_diagnostics` array.

`odin add <name>` creates `<config-dir>/<name>.toml` and refuses to overwrite
an existing file. Values are derived from the name:

- `command = "/opt/odin/bin/<name>"`
- `cwd = "/opt/odin/<name>"`
- `stdout_log = "/var/log/odin/<name>.out.log"`
- `stderr_log = "/var/log/odin/<name>.err.log"`
- `autostart = true`
- `restart = "always"`

The generated file omits `user` and `group`; add them manually when the service
should drop privileges to a dedicated account.

Logs are appended directly to files. Log rotation is intentionally left to
FreeBSD `newsyslog`; see `packaging/freebsd/files/newsyslog/odin.conf`.

Managed services never inherit the caller's stdio. For each app process,
`stdin` is opened from `/dev/null`; configured `stdout_log` and `stderr_log`
receive output, and missing log paths fall back to `/dev/null`. `odin serve`
also detaches its own stdio to `/dev/null` when it starts so it cannot keep a CI
runner's log-capture pipes open. Short-lived control commands such as
`odin restart my-app` still use the caller's stdio and then exit.

`odin stop <service>` first sends `SIGTERM` to the service process group and
waits for `stop_timeout`. If the process group is still present, it sends
`SIGKILL`, records the escalation in the service event history, and reports:

```text
my-app: service stopped after SIGKILL escalation (action=stop, state=stopped, pid=-)
last exit: signal: 9 (SIGKILL)
```

If the process group still exists after `SIGKILL`, `odin stop` fails and prints
the current service status plus recent events so the stuck stop can be diagnosed
without immediately reading service logs.

The JSON error uses the same operation diagnostic shape with `phase =
"sigkill"`:

```json
{
  "code": "operation-failed",
  "message": "service web did not stop after SIGTERM, 10000ms timeout, and SIGKILL; pid=1234",
  "operation": {
    "service": "web",
    "action": "stop",
    "phase": "sigkill",
    "message": "service web did not stop after SIGTERM, 10000ms timeout, and SIGKILL; pid=1234",
    "pid": 1234,
    "state": "stopping",
    "timeout_millis": 10000,
    "recent_events": []
  }
}
```

`odin --json start`, `odin --json stop`, and `odin --json restart` print the
structured operation result on success. Control errors for those commands
include a structured `operation` object. It records the service, action, typed
failure phase, message, current pid/state when known, timeout in milliseconds
when known, and the most recent lifecycle events:

```json
{
  "code": "operation-failed",
  "message": "service web failed startup: state=failed, last_exit=exit status: 42",
  "operation": {
    "service": "web",
    "action": "start",
    "phase": "startup",
    "message": "service web failed startup: state=failed, last_exit=exit status: 42",
    "state": "failed",
    "recent_events": []
  }
}
```

## Control API

The Unix socket protocol starts at `version = 1`. Each request and response is a
single JSON envelope followed by a newline:

```json
{ "version": 1, "command": "start", "service": "web" }
```

Successful control operations return typed response bodies such as `status`,
`reload`, or `operation`. Start, stop, and restart operation results use command
action names: `start`, `stop`, and `restart`.

Errors use one shape for all control commands:

```json
{
  "code": "operation-failed",
  "message": "service web failed startup: state=failed, last_exit=exit status: 42",
  "operation": {
    "service": "web",
    "action": "start",
    "phase": "startup",
    "message": "service web failed startup: state=failed, last_exit=exit status: 42",
    "state": "failed",
    "timeout_millis": 2000,
    "recent_events": []
  },
  "status": {
    "name": "web",
    "state": "failed"
  }
}
```

Optional fields are omitted when absent. `config_diagnostics` is populated for
reload-time config failures. `operation` and `status` are populated for start,
stop, and restart failures when the supervisor can identify the affected service.

Protocol values in v1:

- Request `command`: `status`, `reload`, `start`, `stop`, `restart`
- Response `kind`: `status`, `reload`, `operation`, `ok`, `error`
- Operation `action`: `start`, `stop`, `restart`
- Operation `phase`: `state-check`, `startup`, `stop`, `sigkill`, `runtime`
- Error `code`: `service-not-found`, `already-running`, `not-running`,
  `operation-failed`, `invalid-config`, `duplicate-service`, `toml`,
  `toml-serialize`, `io`, `nix`, `http`, `protocol`, `unsupported-version`
- Service `state`: `stopped`, `starting`, `running`, `stopping`, `failed`,
  `backing-off`
- Health `health`: `unknown`, `healthy`, or an object keyed by `unhealthy` or
  `unready` with the reason string
- Event `kind`: `started`, `exited`, `stopped`, `stop-requested`,
  `restart-scheduled`, `restarted`, `health-changed`, `reload-updated`,
  `reload-restart-required`, `removed`, `added`

## FreeBSD Package

FreeBSD package helper files are in `packaging/freebsd`. The package installs
odin under `/opt/odin`, includes an rc.d script, and installs a newsyslog
configuration.

Build a package on FreeBSD:

```sh
./packaging/freebsd/build.sh
pkg install packaging/freebsd/dist/odin-*.pkg
sysrc odin_enable=YES
service odin start
```

Package builds use the current commit timestamp as a SemVer-compatible version,
formatted as `0.YYYYMMDD.0-tHHMMSS`. When git history is unavailable, set
`ODIN_VERSION_TIMESTAMP` in the build environment with an ISO 8601 value such
as `2026-06-21T01:02:10Z`.

To inspect the fake root without running `pkg create`:

```sh
./packaging/freebsd/build.sh --stage-only
```

## Testing

Normal tests run without root:

```sh
cargo test
```

Privilege dropping has an ignored root-only test because it changes the current
test process uid:

```sh
cargo test --test privilege_drop -- --ignored
```

Run that only in a disposable root test session.
