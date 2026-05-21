//! Typed sandbox errors.

use crate::{CommandId, SandboxId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable error code for clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxErrorCode {
    /// Requested sandbox does not exist.
    SandboxNotFound,
    /// Requested command does not exist within the sandbox.
    CommandNotFound,
    /// Operation is not supported by the active provider.
    Unsupported,
    /// Request failed validation.
    InvalidRequest,
    /// Request conflicts with existing state.
    Conflict,
    /// Provider failed while executing the request.
    Provider,
}

/// Product-neutral local sandbox errors.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SandboxError {
    /// Requested sandbox does not exist.
    #[error("sandbox not found: {sandbox_id}")]
    SandboxNotFound {
        /// Missing sandbox identifier.
        sandbox_id: SandboxId,
    },
    /// Requested command does not exist within the sandbox.
    #[error("command not found: {command_id} in sandbox {sandbox_id}")]
    CommandNotFound {
        /// Sandbox identifier.
        sandbox_id: SandboxId,
        /// Missing command identifier.
        command_id: CommandId,
    },
    /// Operation is not supported by the active provider.
    #[error("unsupported operation: {operation}")]
    Unsupported {
        /// Unsupported operation name.
        operation: String,
    },
    /// Request failed validation.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Validation message.
        message: String,
    },
    /// Request conflicts with existing state.
    #[error("conflict: {message}")]
    Conflict {
        /// Conflict message.
        message: String,
    },
    /// Provider failed while executing the request.
    #[error("provider error: {message}")]
    Provider {
        /// Provider error message.
        message: String,
    },
}

impl SandboxError {
    /// Return the stable error code for this error.
    #[must_use]
    pub const fn code(&self) -> SandboxErrorCode {
        match self {
            Self::SandboxNotFound { .. } => SandboxErrorCode::SandboxNotFound,
            Self::CommandNotFound { .. } => SandboxErrorCode::CommandNotFound,
            Self::Unsupported { .. } => SandboxErrorCode::Unsupported,
            Self::InvalidRequest { .. } => SandboxErrorCode::InvalidRequest,
            Self::Conflict { .. } => SandboxErrorCode::Conflict,
            Self::Provider { .. } => SandboxErrorCode::Provider,
        }
    }

    /// Build an unsupported-operation error.
    #[must_use]
    pub fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
        }
    }

    /// Build an invalid-request error.
    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    /// Build a conflict error.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    /// Build a provider error.
    #[must_use]
    pub fn provider(message: impl Into<String>) -> Self {
        Self::Provider {
            message: message.into(),
        }
    }
}

/// Result type for sandbox operations.
pub type SandboxResult<T> = Result<T, SandboxError>;
