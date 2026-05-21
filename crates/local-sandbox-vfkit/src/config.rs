//! vfkit provider configuration.

use local_sandbox_core::SandboxArchitecture;
use std::{env, path::PathBuf};

const DEFAULT_RUNTIME: &str = "default";

/// Configuration for the vfkit provider implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfkitConfig {
    /// Directory used for per-sandbox disk state.
    pub root_dir: PathBuf,
    /// Architectures this vfkit provider accepts.
    pub supported_architectures: Vec<SandboxArchitecture>,
    /// Runtime profile names this vfkit provider accepts.
    pub supported_runtimes: Vec<String>,
    /// Runtime profile used when a create request omits `runtime`.
    pub default_runtime: String,
    /// Host used when constructing local route URLs.
    pub route_host: String,
}

impl Default for VfkitConfig {
    fn default() -> Self {
        Self {
            root_dir: env::temp_dir().join("local-sandbox-vfkit"),
            supported_architectures: vec![SandboxArchitecture::Arm64],
            supported_runtimes: vec![DEFAULT_RUNTIME.to_string()],
            default_runtime: DEFAULT_RUNTIME.to_string(),
            route_host: "127.0.0.1".to_string(),
        }
    }
}
