# supper

`supper` is a minimal Rust service supervisor for FreeBSD-hosted application processes.
It is intended for long-running third-party apps you own, not for replacing `init`,
`rc.d`, or system service management.

## Layout

- Service configs: `/usr/local/etc/supper/services/*.toml`
- Control socket: `/var/run/supper.sock`
- Service logs: `/var/log/supper`
- Future persistent state, if needed: `/var/db/supper`

## Commands

Run the foreground supervisor:

```sh
supper --config-dir /usr/local/etc/supper/services --socket /var/run/supper.sock monitor
```

Query and control services:

```sh
supper status
supper status my-app
supper start my-app
supper stop my-app
supper restart my-app
```

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

stdout_log = "/var/log/supper/my-app.out.log"
stderr_log = "/var/log/supper/my-app.err.log"

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

Logs are appended directly to files. Log rotation is intentionally left to
FreeBSD `newsyslog`.

## rc.d

An example script is provided at `examples/rc.d/supper`. The supervisor itself
runs in the foreground; the example uses FreeBSD `daemon(8)` to background it
and maintain a pidfile.
