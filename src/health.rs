use std::net::SocketAddr;
use std::process::Stdio;

use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time;

use crate::config::{HealthCheckConfig, HealthCheckKind};
use crate::error::{OdinError, Result};

pub async fn check(config: &HealthCheckConfig) -> Result<()> {
    match config.kind {
        HealthCheckKind::Command => check_command(config).await,
        HealthCheckKind::Tcp => check_tcp(config).await,
        HealthCheckKind::Http => check_http(config).await,
    }
}

async fn check_command(config: &HealthCheckConfig) -> Result<()> {
    let command = config
        .command
        .as_ref()
        .ok_or_else(|| OdinError::Protocol("missing healthcheck command".to_string()))?;
    let mut child = Command::new(command)
        .args(&config.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let status = match time::timeout(config.timeout, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(OdinError::Protocol(
                "healthcheck command timed out".to_string(),
            ));
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(OdinError::Protocol(format!(
            "healthcheck command exited with {status}"
        )))
    }
}

async fn check_tcp(config: &HealthCheckConfig) -> Result<()> {
    let host = config
        .host
        .as_ref()
        .ok_or_else(|| OdinError::Protocol("missing tcp healthcheck host".to_string()))?;
    let port = config
        .port
        .ok_or_else(|| OdinError::Protocol("missing tcp healthcheck port".to_string()))?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|err| OdinError::Protocol(format!("invalid tcp healthcheck address: {err}")))?;
    time::timeout(config.timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| OdinError::Protocol("tcp healthcheck timed out".to_string()))??;
    Ok(())
}

async fn check_http(config: &HealthCheckConfig) -> Result<()> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| OdinError::Protocol("missing http healthcheck url".to_string()))?;
    let client = reqwest::Client::builder().timeout(config.timeout).build()?;
    let response = client.get(url).send().await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(OdinError::Protocol(format!(
            "http healthcheck returned {}",
            response.status()
        )))
    }
}
