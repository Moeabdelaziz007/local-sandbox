//! Serializable sandbox contract types.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* pub struct $name:ident;) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generate a fresh identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Return the underlying `Uuid`.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

id_type! {
    /// Stable sandbox identifier.
    pub struct SandboxId;
}

id_type! {
    /// Stable command identifier within a sandbox.
    pub struct CommandId;
}

id_type! {
    /// Stable snapshot identifier within a sandbox.
    pub struct SnapshotId;
}

/// CPU architecture requested for a sandbox runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxArchitecture {
    /// 64-bit ARM architecture.
    Arm64,
    /// 64-bit x86 architecture.
    X86_64,
}

/// High-level sandbox lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    /// The sandbox record exists but the runtime is not ready yet.
    Created,
    /// The runtime is booting.
    Starting,
    /// The runtime is ready for commands and routes.
    Running,
    /// The runtime is shutting down.
    Stopping,
    /// The runtime is stopped.
    Stopped,
    /// The runtime failed and needs operator attention.
    Failed,
}

/// Provider feature advertised through the capabilities endpoint.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFeature {
    /// Start and inspect commands inside a sandbox.
    Commands,
    /// Write files into a sandbox.
    Files,
    /// Expose sandbox routes to clients.
    Routes,
    /// Extend sandbox lifetime.
    TimeoutExtension,
    /// Create sandbox snapshots.
    Snapshots,
    /// Apply runtime network policy.
    NetworkPolicy,
    /// Spawn userland processes in the sandbox runtime.
    UserlandSpawning,
}

/// Capabilities supported by the current provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    /// Product-neutral provider name.
    pub provider: String,
    /// Architectures accepted by create requests.
    pub architectures: Vec<SandboxArchitecture>,
    /// Provider features available to clients.
    pub features: Vec<SandboxFeature>,
}

/// Network protocol exposed by a route.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxRouteProtocol {
    /// HTTP route.
    Http,
    /// HTTPS route.
    Https,
    /// Raw TCP route.
    Tcp,
}

/// Route exposed by a sandbox runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxRoute {
    /// Stable route label within the sandbox.
    pub name: String,
    /// Protocol used by the route.
    pub protocol: SandboxRouteProtocol,
    /// Target port inside the sandbox.
    pub target_port: u16,
    /// Client-visible URL or address.
    pub url: String,
}

/// Route requested when creating a sandbox.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSandboxRouteRequest {
    /// Stable route label within the sandbox.
    pub name: String,
    /// Protocol used by the route.
    pub protocol: SandboxRouteProtocol,
    /// Target port inside the sandbox.
    pub target_port: u16,
}

/// Runtime resources requested for a sandbox.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxResources {
    /// Virtual CPU count.
    pub vcpus: Option<u16>,
    /// Memory in mebibytes.
    pub memory_mib: Option<u64>,
    /// Disk in mebibytes.
    pub disk_mib: Option<u64>,
}

/// Request to create a sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSandboxRequest {
    /// Optional caller-provided sandbox identifier.
    pub id: Option<SandboxId>,
    /// Optional architecture preference.
    pub architecture: Option<SandboxArchitecture>,
    /// Optional runtime image or profile name.
    pub runtime: Option<String>,
    /// Optional snapshot to restore from.
    pub snapshot_id: Option<SnapshotId>,
    /// Routes to expose when the sandbox is created.
    #[serde(default)]
    pub routes: Vec<CreateSandboxRouteRequest>,
    /// Runtime resources requested for the sandbox.
    pub resources: Option<SandboxResources>,
    /// Whether userland process spawning should be enabled.
    pub userland_spawning: Option<bool>,
    /// Initial timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Environment values to seed into the runtime.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Product-neutral labels for operator tooling.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Sandbox status response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// Sandbox identifier.
    pub id: SandboxId,
    /// Current lifecycle status.
    pub status: SandboxStatus,
    /// Runtime architecture.
    pub architecture: SandboxArchitecture,
    /// Routes exposed by the sandbox.
    pub routes: Vec<SandboxRoute>,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// Expiration timestamp in Unix milliseconds, if configured.
    pub expires_at_unix_ms: Option<u64>,
}

/// Request to start a command in a sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateCommandRequest {
    /// Argument vector to execute.
    pub argv: Vec<String>,
    /// Whether the command should continue running after the request returns.
    #[serde(default)]
    pub detached: bool,
    /// Optional working directory.
    pub cwd: Option<String>,
    /// Environment overrides for the command.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional stdin payload encoded as base64.
    pub stdin_base64: Option<String>,
    /// Optional command timeout in milliseconds.
    pub timeout_ms: Option<u64>,
}

/// Command lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Command is queued for execution.
    Queued,
    /// Command is currently running.
    Running,
    /// Command exited with an exit code.
    Exited,
    /// Command failed before producing a normal exit code.
    Failed,
    /// Command was cancelled by the caller.
    Cancelled,
}

/// Captured command output encoded for JSON transport.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandOutput {
    /// Standard output encoded as base64.
    pub stdout_base64: String,
    /// Standard error encoded as base64.
    pub stderr_base64: String,
}

/// Command status and output response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// Command identifier.
    pub id: CommandId,
    /// Sandbox that owns the command.
    pub sandbox_id: SandboxId,
    /// Current lifecycle status.
    pub status: CommandStatus,
    /// Argument vector used to start the command.
    pub argv: Vec<String>,
    /// Working directory used to start the command.
    pub cwd: Option<String>,
    /// Start timestamp in Unix milliseconds, if started.
    pub started_at_unix_ms: Option<u64>,
    /// Finish timestamp in Unix milliseconds, if finished.
    pub finished_at_unix_ms: Option<u64>,
    /// Process exit code, if the command exited normally.
    pub exit_code: Option<i32>,
    /// Captured command output.
    pub output: CommandOutput,
}

/// Command polling options.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CommandWait {
    /// Whether to wait for command completion before responding.
    pub wait: bool,
    /// Maximum wait time in milliseconds.
    pub timeout_ms: Option<u64>,
}

/// Request to write files into a sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteFilesRequest {
    /// Files to write.
    pub files: Vec<FileWrite>,
}

/// Single file write encoded for JSON transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileWrite {
    /// Sandbox-relative file path.
    pub path: String,
    /// File contents encoded as base64.
    pub contents_base64: String,
    /// Optional Unix file mode.
    pub mode: Option<u32>,
}

/// Response after writing files into a sandbox.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteFilesResponse {
    /// Paths accepted by the provider.
    pub paths: Vec<String>,
}

/// Request to extend a sandbox timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtendSandboxTimeoutRequest {
    /// Additional runtime in milliseconds.
    pub additional_ms: u64,
}

/// Sandbox timeout response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxTimeoutInfo {
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_unix_ms: u64,
}

/// Network access mode requested for a sandbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyMode {
    /// Deny all outbound network access.
    DenyAll,
    /// Allow all outbound network access.
    AllowAll,
    /// Allow only the listed hosts.
    AllowList,
}

/// Request to update sandbox network policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetNetworkPolicyRequest {
    /// Requested policy mode.
    pub mode: NetworkPolicyMode,
    /// Hosts allowed when using allow-list mode.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

/// Request to create a sandbox snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSnapshotRequest {
    /// Optional caller-provided snapshot label.
    pub label: Option<String>,
}

/// Snapshot lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStatus {
    /// Snapshot has been accepted by the provider.
    Created,
    /// Snapshot is being materialized.
    Creating,
    /// Snapshot is ready for restoration.
    Ready,
    /// Snapshot creation failed.
    Failed,
}

/// Snapshot creation response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// Snapshot identifier.
    pub id: SnapshotId,
    /// Sandbox that owns the snapshot.
    pub sandbox_id: SandboxId,
    /// Current lifecycle status.
    pub status: SnapshotStatus,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// Optional caller-provided label.
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_status_and_architecture_as_snake_case() {
        assert_eq!(
            serde_json::to_value(SandboxStatus::Starting).unwrap(),
            json!("starting")
        );
        assert_eq!(
            serde_json::to_value(SandboxArchitecture::X86_64).unwrap(),
            json!("x86_64")
        );
    }

    #[test]
    fn serializes_ids_as_json_strings() {
        let id = SandboxId::new();
        let value = serde_json::to_value(id).unwrap();
        assert_eq!(value, json!(id.to_string()));
    }

    #[test]
    fn serializes_file_writes_with_base64_payloads() {
        let request = WriteFilesRequest {
            files: vec![FileWrite {
                path: "/workspace/app/page.tsx".to_string(),
                contents_base64: "SGVsbG8=".to_string(),
                mode: Some(0o644),
            }],
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "files": [{
                    "path": "/workspace/app/page.tsx",
                    "contents_base64": "SGVsbG8=",
                    "mode": 420
                }]
            })
        );
    }

    #[test]
    fn serializes_create_request_adapter_fields() {
        let snapshot_id = SnapshotId::new();
        let request = CreateSandboxRequest {
            id: None,
            architecture: Some(SandboxArchitecture::Arm64),
            runtime: Some("node-22".to_string()),
            snapshot_id: Some(snapshot_id),
            routes: vec![CreateSandboxRouteRequest {
                name: "preview".to_string(),
                protocol: SandboxRouteProtocol::Https,
                target_port: 3000,
            }],
            resources: Some(SandboxResources {
                vcpus: Some(4),
                memory_mib: Some(8192),
                disk_mib: None,
            }),
            userland_spawning: Some(true),
            timeout_ms: Some(60_000),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "id": null,
                "architecture": "arm64",
                "runtime": "node-22",
                "snapshot_id": snapshot_id.to_string(),
                "routes": [{
                    "name": "preview",
                    "protocol": "https",
                    "target_port": 3000
                }],
                "resources": {
                    "vcpus": 4,
                    "memory_mib": 8192,
                    "disk_mib": null
                },
                "userland_spawning": true,
                "timeout_ms": 60000,
                "env": {},
                "labels": {}
            })
        );
    }

    #[test]
    fn serializes_detached_command_flag() {
        let request = CreateCommandRequest {
            argv: vec!["pnpm".to_string(), "dev".to_string()],
            detached: true,
            cwd: Some("/workspace".to_string()),
            env: BTreeMap::new(),
            stdin_base64: None,
            timeout_ms: None,
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "argv": ["pnpm", "dev"],
                "detached": true,
                "cwd": "/workspace",
                "env": {},
                "stdin_base64": null,
                "timeout_ms": null
            })
        );
    }
}
