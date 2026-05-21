//! Provider state for vfkit sandbox lifecycle.

use crate::{disk::DiskHandle, process::ProcessHandle};
use local_sandbox_core::{SandboxId, SandboxInfo};
use std::collections::BTreeMap;

/// In-memory lifecycle state for the vfkit provider.
#[derive(Default)]
pub(crate) struct VfkitState {
    pub(crate) sandboxes: BTreeMap<SandboxId, SandboxRecord>,
}

/// Lifecycle record for a single sandbox.
pub(crate) struct SandboxRecord {
    pub(crate) info: SandboxInfo,
    pub(crate) disk: Option<DiskHandle>,
    pub(crate) process: Option<ProcessHandle>,
}

impl SandboxRecord {
    #[must_use]
    pub(crate) const fn new(info: SandboxInfo) -> Self {
        Self {
            info,
            disk: None,
            process: None,
        }
    }
}
