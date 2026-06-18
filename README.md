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
supper --json status
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

`supper monitor` handles `SIGHUP` by reloading the config directory. Reloading:

- adds new services and starts them when `autostart = true`
- updates stored configuration for existing services
- stops and removes services deleted from the config directory
- keeps already-running services running when their config still exists

Logs are appended directly to files. Log rotation is intentionally left to
FreeBSD `newsyslog`; see `examples/newsyslog/supper.conf`.

## rc.d

An example script is provided at `examples/rc.d/supper`. The supervisor itself
runs in the foreground; the example uses FreeBSD `daemon(8)` to background it
and maintain a pidfile.

Install sketch:

```sh
install -m 0755 target/release/supper /usr/local/bin/supper
install -d -m 0755 /usr/local/etc/supper/services
install -d -m 0755 /var/log/supper
install -m 0755 examples/rc.d/supper /usr/local/etc/rc.d/supper
sysrc supper_enable=YES
service supper start
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
