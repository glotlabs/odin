use std::fs;
use std::path::Path;
use std::process::Command;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

fn main() {
    configure_git_rerun();
    println!("cargo:rerun-if-env-changed=ODIN_VERSION");
    println!("cargo:rerun-if-env-changed=ODIN_VERSION_TIMESTAMP");

    let version = std::env::var("ODIN_VERSION")
        .ok()
        .or_else(env_timestamp_version)
        .or_else(git_timestamp_version)
        .unwrap_or_else(|| {
            std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
        });

    println!("cargo:rustc-env=ODIN_VERSION={version}");
}

fn env_timestamp_version() -> Option<String> {
    let timestamp = std::env::var("ODIN_VERSION_TIMESTAMP").ok()?;
    timestamp_to_semver(timestamp.trim())
}

fn git_timestamp_version() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let output = Command::new("git")
        .args(["-C", &manifest_dir, "show", "-s", "--format=%cI", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let timestamp = String::from_utf8(output.stdout).ok()?;
    timestamp_to_semver(timestamp.trim())
}

fn timestamp_to_semver(timestamp: &str) -> Option<String> {
    let timestamp = OffsetDateTime::parse(timestamp, &Rfc3339).ok()?;
    let timestamp = timestamp.to_offset(UtcOffset::UTC);

    Some(format!(
        "0.{:04}{:02}{:02}.0-t{:02}{:02}{:02}",
        timestamp.year(),
        timestamp.month() as u8,
        timestamp.day(),
        timestamp.hour(),
        timestamp.minute(),
        timestamp.second()
    ))
}

fn configure_git_rerun() {
    println!("cargo:rerun-if-changed=.git/HEAD");

    let Ok(head) = fs::read_to_string(".git/HEAD") else {
        return;
    };
    let Some(ref_name) = head.trim().strip_prefix("ref: ") else {
        return;
    };

    let ref_path = Path::new(".git").join(ref_name);
    println!("cargo:rerun-if-changed={}", ref_path.display());
}
