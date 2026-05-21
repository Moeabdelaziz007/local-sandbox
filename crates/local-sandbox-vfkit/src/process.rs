//! Process lifecycle seam for vfkit sandboxes.

use local_sandbox_core::{
    SandboxArchitecture, SandboxError, SandboxId, SandboxResources, SandboxResult, SnapshotId,
};
use std::path::PathBuf;

/// Runtime process start request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessSpec {
    pub(crate) sandbox_id: SandboxId,
    pub(crate) architecture: SandboxArchitecture,
    pub(crate) runtime: String,
    pub(crate) disk_path: PathBuf,
    pub(crate) snapshot_id: Option<SnapshotId>,
    pub(crate) resources: Option<SandboxResources>,
}

/// Runtime process handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessHandle {
    pub(crate) sandbox_id: SandboxId,
    pub(crate) token: String,
    pub(crate) agent_socket_path: Option<PathBuf>,
}

/// Fakeable vfkit process boundary.
pub(crate) trait ProcessSpawner: Send + Sync {
    fn start(&self, spec: &ProcessSpec) -> SandboxResult<ProcessHandle>;

    fn stop(&self, handle: &ProcessHandle) -> SandboxResult<()>;
}

/// Deterministic process spawner used until real vfkit boot is wired in.
#[derive(Default)]
pub(crate) struct NoopProcessSpawner;

impl ProcessSpawner for NoopProcessSpawner {
    fn start(&self, spec: &ProcessSpec) -> SandboxResult<ProcessHandle> {
        let snapshot = spec
            .snapshot_id
            .map_or_else(|| "none".to_string(), |snapshot_id| snapshot_id.to_string());
        Ok(ProcessHandle {
            sandbox_id: spec.sandbox_id,
            token: format!(
                "noop:{}:{:?}:{}:{}:{}:{:?}",
                spec.sandbox_id,
                spec.architecture,
                spec.runtime,
                spec.disk_path.display(),
                snapshot,
                spec.resources
            ),
            agent_socket_path: None,
        })
    }

    fn stop(&self, handle: &ProcessHandle) -> SandboxResult<()> {
        if handle.token.is_empty() {
            Err(SandboxError::provider("process handle token is empty"))
        } else {
            Ok(())
        }
    }
}
