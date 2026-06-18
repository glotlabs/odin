use nix::sys::stat::{Mode, umask};
use nix::unistd::setsid;
use tokio::process::{Child, Command};

use crate::config::ServiceConfig;
use crate::error::Result;
use crate::logging;
use crate::privileges::Privileges;

pub fn spawn_service(config: &ServiceConfig) -> Result<Child> {
    logging::prepare_log_dirs(config)?;
    let logs = logging::open_log_files(config)?;
    let privileges = Privileges::resolve(config)?;

    let mut command = Command::new(&config.command);
    command.args(&config.args);
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }
    command.envs(&config.env);
    if let Some(stdout) = logs.stdout {
        command.stdout(stdout);
    }
    if let Some(stderr) = logs.stderr {
        command.stderr(stderr);
    }

    let umask_value = config.umask;
    unsafe {
        command.pre_exec(move || {
            setsid().map_err(std::io::Error::other)?;
            if let Some(mask) = umask_value {
                umask(Mode::from_bits_truncate(mask as nix::libc::mode_t));
            }
            privileges.apply().map_err(std::io::Error::other)?;
            Ok(())
        });
    }

    Ok(command.spawn()?)
}
