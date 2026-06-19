use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::config::ConfigDiagnostic;
use crate::error::{OdinError, Result};
use crate::status::{ServiceEvent, ServiceState, ServiceStatus};
use crate::supervisor::{
    OperationAction, OperationFailure, OperationPhase, OperationResult, ReloadSummary,
    SupervisorHandle,
};

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
    Operation { result: OperationResult },
    Ok,
    Error { error: ControlError },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_diagnostics: Option<Vec<ConfigDiagnostic>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationDiagnostic>,
    pub status: Option<ServiceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationDiagnostic {
    pub service: String,
    pub action: OperationAction,
    pub phase: OperationPhase,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<ServiceState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_events: Vec<ServiceEvent>,
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
    .map_err(|err| OdinError::Protocol(err.to_string()))?;
    stream.write_all(&encoded).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let envelope: ControlResponseEnvelope =
        serde_json::from_str(&line).map_err(|err| OdinError::Protocol(err.to_string()))?;
    if envelope.version != CONTROL_VERSION {
        return Err(OdinError::Protocol(format!(
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
        serde_json::from_str(&line).map_err(|err| OdinError::Protocol(err.to_string()))?;
    let response = if envelope.version == CONTROL_VERSION {
        dispatch(envelope.request, config_dir, supervisor).await
    } else {
        ControlResponse::Error {
            error: ControlError {
                code: "unsupported-version".to_string(),
                message: format!("unsupported control request version: {}", envelope.version),
                config_diagnostics: None,
                operation: None,
                status: None,
            },
        }
    };
    let payload = serde_json::to_vec(&ControlResponseEnvelope {
        version: CONTROL_VERSION,
        response,
    })
    .map_err(|err| OdinError::Protocol(err.to_string()))?;
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
        ControlRequest::Start { service } => {
            return operation_response(
                supervisor.clone(),
                &service,
                OperationAction::Start,
                supervisor.start(&service).await,
            )
            .await;
        }
        ControlRequest::Stop { service } => {
            return operation_response(
                supervisor.clone(),
                &service,
                OperationAction::Stop,
                supervisor.stop(&service).await,
            )
            .await;
        }
        ControlRequest::Restart { service } => {
            return operation_response(
                supervisor.clone(),
                &service,
                OperationAction::Restart,
                supervisor.restart(&service).await,
            )
            .await;
        }
    };
    match result {
        Ok(response) => response,
        Err(err) => ControlResponse::Error {
            error: control_error(&err, None, None),
        },
    }
}

async fn operation_response(
    supervisor: SupervisorHandle,
    service: &str,
    action: OperationAction,
    result: Result<OperationResult>,
) -> ControlResponse {
    match result {
        Ok(result) => ControlResponse::Operation { result },
        Err(err) => {
            let status = supervisor
                .status(Some(service))
                .await
                .ok()
                .and_then(|mut statuses| statuses.pop());
            let operation = operation_diagnostic(service, action, &err, status.as_ref());
            ControlResponse::Error {
                error: control_error(&err, status, operation),
            }
        }
    }
}

fn control_error(
    err: &OdinError,
    status: Option<ServiceStatus>,
    operation: Option<OperationDiagnostic>,
) -> ControlError {
    let config_diagnostics = match err {
        OdinError::ConfigDiagnostics(diagnostics) => Some(diagnostics.diagnostics.clone()),
        _ => None,
    };
    ControlError {
        code: error_code(err).to_string(),
        message: err.to_string(),
        config_diagnostics,
        operation,
        status,
    }
}

fn operation_diagnostic(
    service: &str,
    action: OperationAction,
    err: &OdinError,
    status: Option<&ServiceStatus>,
) -> Option<OperationDiagnostic> {
    if matches!(err, OdinError::ConfigDiagnostics(_)) {
        return None;
    }
    let failure = match err {
        OdinError::OperationFailure(failure) => Some(failure),
        _ => None,
    };
    let message = failure
        .map(|failure| failure.message.clone())
        .unwrap_or_else(|| err.to_string());
    let recent_events = status
        .map(|status| {
            let start = status.event_history.len().saturating_sub(5);
            status.event_history[start..].to_vec()
        })
        .unwrap_or_default();

    Some(OperationDiagnostic {
        service: service.to_string(),
        action,
        phase: operation_phase(action, err, failure, status),
        message: message.clone(),
        pid: failure
            .and_then(|failure| failure.pid)
            .or_else(|| status.and_then(|status| status.pid)),
        state: status.map(|status| status.state),
        timeout_millis: failure.and_then(|failure| failure.timeout_millis),
        recent_events,
    })
}

fn operation_phase(
    action: OperationAction,
    err: &OdinError,
    failure: Option<&OperationFailure>,
    status: Option<&ServiceStatus>,
) -> OperationPhase {
    if let Some(failure) = failure {
        return failure.phase;
    }
    if matches!(
        err,
        OdinError::ServiceNotFound(_) | OdinError::AlreadyRunning(_) | OdinError::NotRunning(_)
    ) {
        return OperationPhase::StateCheck;
    }
    match action {
        OperationAction::Start | OperationAction::Restart => OperationPhase::Startup,
        OperationAction::Stop => {
            if status.is_some_and(|status| status.state == ServiceState::Stopping) {
                OperationPhase::Stop
            } else {
                OperationPhase::Runtime
            }
        }
    }
}

fn error_code(err: &OdinError) -> &'static str {
    match err {
        OdinError::ServiceNotFound(_) => "service-not-found",
        OdinError::AlreadyRunning(_) => "already-running",
        OdinError::NotRunning(_) => "not-running",
        OdinError::OperationFailure(_) => "operation-failed",
        OdinError::ConfigDiagnostics(_) => "invalid-config",
        OdinError::InvalidConfig { .. } => "invalid-config",
        OdinError::DuplicateService(_) => "duplicate-service",
        OdinError::Toml { .. } => "toml",
        OdinError::TomlSerialize(_) => "toml-serialize",
        OdinError::Io(_) => "io",
        OdinError::Nix(_) => "nix",
        OdinError::Http(_) => "http",
        OdinError::Protocol(_) => "protocol",
    }
}
