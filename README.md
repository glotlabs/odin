# odin

`odin` is a minimal Rust service supervisor for FreeBSD-hosted application processes.
It is intended for long-running third-party apps you own, not for replacing `init`,
`rc.d`, or system service management.

## Layout

- Service configs: `/usr/local/etc/odin/services/*.toml`
- Control socket: `/var/run/odin.sock`
- Service logs: `/var/log/odin`
- Future persistent state, if needed: `/var/db/odin`

## Commands

Run the foreground supervisor:

```sh
odin --config-dir /usr/local/etc/odin/services --socket /var/run/odin.sock monitor
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

`odin status` includes the most recent restart reason in the human table.
`odin --json status` includes bounded restart history for each service. The
history keeps the last 64 restart records with timestamp, reason, previous PID,
new PID, exit text when available, and backoff delay in milliseconds.

`odin --json status` also includes bounded event history for each service. The
event history keeps the last 128 lifecycle records, including starts, exits,
stops, scheduled restarts, reload updates, reload-required restarts, removals,
and health state changes. Use `odin events <service>` for a compact human
view of one service's event history.

## Service Config

```toml
name = "my-app"
command = "/usr/local/bin/my-app"
args = ["--port", "8080"]
cwd = "/usr/local/my-app"
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

`odin monitor` handles `SIGHUP` by reloading the config directory. Reloading:

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
require finding the monitor PID or using rc.d-specific commands.

Process-affecting fields are `command`, `args`, `cwd`, `env`, `user`, `group`,
`umask`, `stdout_log`, and `stderr_log`. Restart policy, restart delays, stop
timeout, autostart, and health-check changes are live updates.

`odin validate` checks all service config files without starting anything. It
parses TOML, checks duplicate names, validates user/group lookups, verifies
absolute command paths exist, checks `cwd`, and reports log directories that
will be created by the monitor.

Config errors are reported with the config file, field, source line, caret, and
a short help message when the location is known:

```text
error: /usr/local/etc/odin/services/web.toml:3:11 command: command must be an absolute path
     3 | command = "web"
       |           ^
  help: Set command to an absolute path such as "/usr/local/bin/app".
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
      "path": "/usr/local/etc/odin/services/web.toml",
      "line": 3,
      "column": 11,
      "service": "web",
      "field": "command",
      "message": "command must be an absolute path",
      "help": "Set command to an absolute path such as \"/usr/local/bin/app\"."
    }
  ],
  "warnings": [],
  "diagnostics": [
    {
      "severity": "error",
      "path": "/usr/local/etc/odin/services/web.toml",
      "line": 3,
      "column": 11,
      "service": "web",
      "field": "command",
      "message": "command must be an absolute path",
      "help": "Set command to an absolute path such as \"/usr/local/bin/app\"."
    }
  ]
}
```

Reload uses the same diagnostics. If `odin reload` asks the monitor to reload an
invalid config directory, the command prints source/caret diagnostics and exits
non-zero. `odin --json reload` returns a structured control error with
`code = "invalid-config"` and a `diagnostics` array.

`odin add <name>` creates `<config-dir>/<name>.toml` and refuses to overwrite
an existing file. Values are derived from the name:

- `command = "/usr/local/bin/<name>"`
- `cwd = "/usr/local/<name>"`
- `stdout_log = "/var/log/odin/<name>.out.log"`
- `stderr_log = "/var/log/odin/<name>.err.log"`
- `autostart = true`
- `restart = "always"`

The generated file omits `user` and `group`; add them manually when the service
should drop privileges to a dedicated account.

Logs are appended directly to files. Log rotation is intentionally left to
FreeBSD `newsyslog`; see `examples/newsyslog/odin.conf`.

Managed services never inherit the caller's stdio. For each app process,
`stdin` is opened from `/dev/null`; configured `stdout_log` and `stderr_log`
receive output, and missing log paths fall back to `/dev/null`. `odin monitor`
also detaches its own stdio to `/dev/null` when it starts so it cannot keep a CI
runner's log-capture pipes open. Short-lived control commands such as
`odin restart my-app` still use the caller's stdio and then exit.

`odin stop <service>` first sends `SIGTERM` to the service process group and
waits for `stop_timeout`. If the process group is still present, it sends
`SIGKILL`, records the escalation in the service event history, and reports:

```text
my-app: service stopped after SIGKILL escalation (Stopped, pid=-)
last exit: signal: 9 (SIGKILL)
```

If the process group still exists after `SIGKILL`, `odin stop` fails and prints
the current service status plus recent events so the stuck stop can be diagnosed
without immediately reading service logs.

## rc.d

An example script is provided at `examples/rc.d/odin`. The supervisor itself
runs in the foreground; the example uses FreeBSD `daemon(8)` to background it
and maintain a pidfile.

Install sketch:

```sh
install -m 0755 target/release/odin /usr/local/bin/odin
install -d -m 0755 /usr/local/etc/odin/services
install -d -m 0755 /var/log/odin
install -m 0755 examples/rc.d/odin /usr/local/etc/rc.d/odin
sysrc odin_enable=YES
service odin start
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
