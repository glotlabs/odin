use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::Stdio;

use crate::config::ServiceConfig;
use crate::error::Result;

pub struct LogFiles {
    pub stdout: Option<Stdio>,
    pub stderr: Option<Stdio>,
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
        stdout: service
            .stdout_log
            .as_ref()
            .map(|path| open_append(path))
            .transpose()?
            .map(Stdio::from),
        stderr: service
            .stderr_log
            .as_ref()
            .map(|path| open_append(path))
            .transpose()?
            .map(Stdio::from),
    })
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
