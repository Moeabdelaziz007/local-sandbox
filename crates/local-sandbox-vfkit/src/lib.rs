//! vfkit provider boundary.

use local_sandbox_core::{CreateSandboxRequest, SandboxArchitecture, SandboxInfo, SandboxStatus};
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal vfkit provider placeholder.
#[derive(Debug, Default)]
pub struct VfkitProvider;

impl VfkitProvider {
    /// Create a provider instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Create a sandbox record.
    #[must_use]
    pub fn create(&self, request: &CreateSandboxRequest) -> SandboxInfo {
        let now = unix_time_ms();
        SandboxInfo {
            id: request.id.unwrap_or_default(),
            status: SandboxStatus::Created,
            architecture: request.architecture.unwrap_or(SandboxArchitecture::Arm64),
            routes: Vec::new(),
            created_at_unix_ms: now,
            expires_at_unix_ms: request
                .timeout_ms
                .map(|timeout| now.saturating_add(timeout)),
        }
    }
}

fn unix_time_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
