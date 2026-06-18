use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::error::{Result, SupperError};
use crate::status::ServiceStatus;
use crate::supervisor::SupervisorHandle;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum ControlRequest {
    Status { service: Option<String> },
    Start { service: String },
    Stop { service: String },
    Restart { service: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlResponse {
    Status { services: Vec<ServiceStatus> },
    Ok,
    Error { message: String },
}

pub async fn serve(path: &Path, supervisor: SupervisorHandle) -> Result<()> {
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
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, supervisor).await {
                tracing::warn!("control connection failed: {err}");
            }
        });
    }
}

pub async fn request(path: &Path, request: ControlRequest) -> Result<ControlResponse> {
    let mut stream = UnixStream::connect(path).await?;
    let encoded =
        serde_json::to_vec(&request).map_err(|err| SupperError::Protocol(err.to_string()))?;
    stream.write_all(&encoded).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    serde_json::from_str(&line).map_err(|err| SupperError::Protocol(err.to_string()))
}

async fn handle_connection(stream: UnixStream, supervisor: SupervisorHandle) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: ControlRequest =
        serde_json::from_str(&line).map_err(|err| SupperError::Protocol(err.to_string()))?;
    let response = dispatch(request, supervisor).await;
    let payload =
        serde_json::to_vec(&response).map_err(|err| SupperError::Protocol(err.to_string()))?;
    let stream = reader.get_mut();
    stream.write_all(&payload).await?;
    stream.write_all(b"\n").await?;
    Ok(())
}

async fn dispatch(request: ControlRequest, supervisor: SupervisorHandle) -> ControlResponse {
    let result = match request {
        ControlRequest::Status { service } => supervisor
            .status(service.as_deref())
            .await
            .map(|services| ControlResponse::Status { services }),
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
            message: err.to_string(),
        },
    }
}
