//! Provider-neutral local sandbox contracts.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Stable sandbox identifier.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SandboxId(Uuid);

impl SandboxId {
    /// Generate a fresh sandbox identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SandboxId {
    fn default() -> Self {
        Self::new()
    }
}

/// High-level sandbox lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    /// The sandbox record exists but the runtime is not booted.
    Created,
    /// The runtime is booting.
    Starting,
    /// The runtime is ready for commands/routes.
    Running,
    /// The runtime is shutting down.
    Stopping,
    /// The runtime is stopped.
    Stopped,
    /// The runtime failed and needs operator attention.
    Error,
}

/// Request to create a sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSandboxRequest {
    /// Optional human-readable name for logs and operator UIs.
    pub name: Option<String>,
}

/// Sandbox status response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// Sandbox identifier.
    pub id: SandboxId,
    /// Current status.
    pub status: SandboxStatus,
}

/// Product-neutral local sandbox errors.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Requested sandbox does not exist.
    #[error("sandbox not found: {0:?}")]
    NotFound(SandboxId),
    /// Provider failed to complete an operation.
    #[error("provider error: {0}")]
    Provider(String),
}

/// Result type for sandbox operations.
pub type SandboxResult<T> = Result<T, SandboxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_unique_sandbox_ids() {
        assert_ne!(SandboxId::new(), SandboxId::new());
    }

    #[test]
    fn serializes_status_as_snake_case() {
        let json = serde_json::to_string(&SandboxStatus::Starting).unwrap();
        assert_eq!(json, "\"starting\"");
    }
}
