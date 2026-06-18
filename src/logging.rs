use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Stdio;

use crate::config::ServiceConfig;
use crate::error::Result;

pub struct LogFiles {
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

pub fn prepare_log_dirs(service: &ServiceConfig) -> Result<()> {
    if let Some(path) = &service.stdout_log {
        create_parent(path)?;
    }
    if let Some(path) = &service.stderr_log {
        create_parent(path)?;
    }
    Ok(())
}

pub fn open_log_files(service: &ServiceConfig) -> Result<LogFiles> {
    Ok(LogFiles {
        stdin: Stdio::from(open_read("/dev/null")?),
        stdout: Stdio::from(open_output_or_null(service.stdout_log.as_deref())?),
        stderr: Stdio::from(open_output_or_null(service.stderr_log.as_deref())?),
    })
}

pub fn detach_process_stdio() -> Result<()> {
    let stdin = open_read("/dev/null")?;
    let stdout = open_write("/dev/null")?;
    let stderr = open_write("/dev/null")?;

    dup2(&stdin, nix::libc::STDIN_FILENO)?;
    dup2(&stdout, nix::libc::STDOUT_FILENO)?;
    dup2(&stderr, nix::libc::STDERR_FILENO)?;
    Ok(())
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn open_append(path: &Path) -> Result<File> {
    create_parent(path)?;
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

fn open_output_or_null(path: Option<&Path>) -> Result<File> {
    match path {
        Some(path) => open_append(path),
        None => open_write("/dev/null"),
    }
}

fn open_read(path: impl AsRef<Path>) -> Result<File> {
    Ok(OpenOptions::new().read(true).open(path)?)
}

fn open_write(path: impl AsRef<Path>) -> Result<File> {
    Ok(OpenOptions::new().write(true).open(path)?)
}

fn dup2(file: &File, target_fd: nix::libc::c_int) -> Result<()> {
    if unsafe { nix::libc::dup2(file.as_raw_fd(), target_fd) } == -1 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}
