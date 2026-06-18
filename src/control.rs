use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::error::{Result, SupperError};
use crate::status::ServiceStatus;
use crate::supervisor::{ReloadSummary, SupervisorHandle};

pub const CONTROL_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlEnvelope {
    pub version: u16,
    #[serde(flatten)]
    pub request: ControlRequest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum ControlRequest {
    Status { service: Option<String> },
    Reload,
    Start { service: String },
    Stop { service: String },
    Restart { service: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlResponseEnvelope {
    pub version: u16,
    #[serde(flatten)]
    pub response: ControlResponse,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlResponse {
    Status { services: Vec<ServiceStatus> },
    Reload { summary: ReloadSummary },
    Ok,
    Error { error: ControlError },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

pub async fn serve(path: &Path, config_dir: PathBuf, supervisor: SupervisorHandle) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(path)?;
    loop {
        let (stream, _) = listener.accept().await?;
        let supervisor = supervisor.clone();
        let config_dir = config_dir.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, &config_dir, supervisor).await {
                tracing::warn!("control connection failed: {err}");
            }
        });
    }
}

pub async fn request(path: &Path, request: ControlRequest) -> Result<ControlResponse> {
    let mut stream = UnixStream::connect(path).await?;
    let encoded = serde_json::to_vec(&ControlEnvelope {
        version: CONTROL_VERSION,
        request,
    })
    .map_err(|err| SupperError::Protocol(err.to_string()))?;
    stream.write_all(&encoded).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let envelope: ControlResponseEnvelope =
        serde_json::from_str(&line).map_err(|err| SupperError::Protocol(err.to_string()))?;
    if envelope.version != CONTROL_VERSION {
        return Err(SupperError::Protocol(format!(
            "unsupported control response version: {}",
            envelope.version
        )));
    }
    Ok(envelope.response)
}

async fn handle_connection(
    stream: UnixStream,
    config_dir: &Path,
    supervisor: SupervisorHandle,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let envelope: ControlEnvelope =
        serde_json::from_str(&line).map_err(|err| SupperError::Protocol(err.to_string()))?;
    let response = if envelope.version == CONTROL_VERSION {
        dispatch(envelope.request, config_dir, supervisor).await
    } else {
        ControlResponse::Error {
            error: ControlError {
                code: "unsupported-version".to_string(),
                message: format!("unsupported control request version: {}", envelope.version),
            },
        }
    };
    let payload = serde_json::to_vec(&ControlResponseEnvelope {
        version: CONTROL_VERSION,
        response,
    })
    .map_err(|err| SupperError::Protocol(err.to_string()))?;
    let stream = reader.get_mut();
    stream.write_all(&payload).await?;
    stream.write_all(b"\n").await?;
    Ok(())
}

async fn dispatch(
    request: ControlRequest,
    config_dir: &Path,
    supervisor: SupervisorHandle,
) -> ControlResponse {
    let result = match request {
        ControlRequest::Status { service } => supervisor
            .status(service.as_deref())
            .await
            .map(|services| ControlResponse::Status { services }),
        ControlRequest::Reload => match crate::config::load_services(config_dir) {
            Ok(services) => supervisor
                .reload(services)
                .await
                .map(|summary| ControlResponse::Reload { summary }),
            Err(err) => Err(err),
        },
        ControlRequest::Start { service } => supervisor
            .start(&service)
            .await
            .map(|_| ControlResponse::Ok),
        ControlRequest::Stop { service } => {
            supervisor.stop(&service).await.map(|_| ControlResponse::Ok)
        }
        ControlRequest::Restart { service } => supervisor
            .restart(&service)
            .await
            .map(|_| ControlResponse::Ok),
    };
    match result {
        Ok(response) => response,
        Err(err) => ControlResponse::Error {
            error: ControlError {
                code: error_code(&err).to_string(),
                message: err.to_string(),
            },
        },
    }
}

fn error_code(err: &SupperError) -> &'static str {
    match err {
        SupperError::ServiceNotFound(_) => "service-not-found",
        SupperError::AlreadyRunning(_) => "already-running",
        SupperError::NotRunning(_) => "not-running",
        SupperError::InvalidConfig { .. } => "invalid-config",
        SupperError::DuplicateService(_) => "duplicate-service",
        SupperError::Toml { .. } => "toml",
        SupperError::TomlSerialize(_) => "toml-serialize",
        SupperError::Io(_) => "io",
        SupperError::Nix(_) => "nix",
        SupperError::Http(_) => "http",
        SupperError::Protocol(_) => "protocol",
    }
}
