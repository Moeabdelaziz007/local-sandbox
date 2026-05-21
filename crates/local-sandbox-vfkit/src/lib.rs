//! vfkit provider boundary.

use local_sandbox_core::{CreateSandboxRequest, SandboxId, SandboxInfo, SandboxStatus};

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
    pub fn create(&self, _request: CreateSandboxRequest) -> SandboxInfo {
        SandboxInfo {
            id: SandboxId::new(),
            status: SandboxStatus::Created,
        }
    }
}
