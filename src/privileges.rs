use std::ffi::CString;

use nix::unistd::{Gid, Group, Uid, User, setgid, setuid};

use crate::config::ServiceConfig;
use crate::error::{Result, SupperError};

#[derive(Debug, Clone)]
pub struct Privileges {
    pub user: Option<User>,
    pub group: Option<Group>,
}

impl Privileges {
    pub fn resolve(config: &ServiceConfig) -> Result<Self> {
        let user = match &config.user {
            Some(name) => Some(
                User::from_name(name)?
                    .ok_or_else(|| SupperError::Protocol(format!("unknown user: {name}")))?,
            ),
            None => None,
        };
        let group = match &config.group {
            Some(name) => Some(
                Group::from_name(name)?
                    .ok_or_else(|| SupperError::Protocol(format!("unknown group: {name}")))?,
            ),
            None => None,
        };
        Ok(Self { user, group })
    }

    pub fn apply(&self) -> Result<()> {
        let gid = self
            .group
            .as_ref()
            .map(|group| group.gid)
            .or_else(|| self.user.as_ref().map(|user| user.gid));

        if let Some(user) = &self.user {
            let name = CString::new(user.name.clone())
                .map_err(|_| SupperError::Protocol("user name contains NUL".to_string()))?;
            init_supplementary_groups(&name, gid.unwrap_or(user.gid))?;
        }

        if let Some(gid) = gid {
            setgid(Gid::from_raw(gid.as_raw()))?;
        }
        if let Some(user) = &self.user {
            setuid(Uid::from_raw(user.uid.as_raw()))?;
        }
        Ok(())
    }
}

#[cfg(target_os = "freebsd")]
fn init_supplementary_groups(user: &std::ffi::CStr, group: Gid) -> Result<()> {
    let rc = unsafe { nix::libc::initgroups(user.as_ptr(), group.as_raw()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SupperError::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(not(target_os = "freebsd"))]
fn init_supplementary_groups(_user: &std::ffi::CStr, _group: Gid) -> Result<()> {
    Ok(())
}
