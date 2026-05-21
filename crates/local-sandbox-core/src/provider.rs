//! Provider trait and in-memory implementation.

use crate::{
    CommandId, CommandInfo, CommandOutput, CommandStatus, CommandWait, CreateCommandRequest,
    CreateSandboxRequest, CreateSandboxRouteRequest, CreateSnapshotRequest,
    ExtendSandboxTimeoutRequest, FileWrite, SandboxArchitecture, SandboxCapabilities, SandboxError,
    SandboxFeature, SandboxId, SandboxInfo, SandboxResult, SandboxRoute, SandboxStatus,
    SandboxTimeoutInfo, SetNetworkPolicyRequest, SnapshotId, SnapshotInfo, SnapshotStatus,
    WriteFilesRequest, WriteFilesResponse,
};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// Future returned by sandbox provider operations.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = SandboxResult<T>> + Send + 'a>>;

const MAX_FILES_PER_WRITE: usize = 4096;

/// Product-neutral sandbox provider boundary.
pub trait SandboxProvider: Send + Sync + 'static {
    /// Return provider capabilities.
    fn capabilities(&self) -> ProviderFuture<'_, SandboxCapabilities>;

    /// Create a new sandbox.
    fn create_sandbox(&self, request: CreateSandboxRequest) -> ProviderFuture<'_, SandboxInfo>;

    /// Get current sandbox information.
    fn get_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo>;

    /// Delete a sandbox.
    fn delete_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo>;

    /// Start a command in a sandbox.
    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
    ) -> ProviderFuture<'_, CommandInfo>;

    /// Get command status and output.
    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        wait: CommandWait,
    ) -> ProviderFuture<'_, CommandInfo>;

    /// Cancel a command.
    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
    ) -> ProviderFuture<'_, CommandInfo>;

    /// Write files into a sandbox.
    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> ProviderFuture<'_, WriteFilesResponse>;

    /// Extend sandbox timeout.
    fn extend_timeout(
        &self,
        sandbox_id: SandboxId,
        request: ExtendSandboxTimeoutRequest,
    ) -> ProviderFuture<'_, SandboxTimeoutInfo>;

    /// Apply sandbox network policy.
    fn set_network_policy(
        &self,
        sandbox_id: SandboxId,
        request: SetNetworkPolicyRequest,
    ) -> ProviderFuture<'_, ()>;

    /// Create a sandbox snapshot.
    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
    ) -> ProviderFuture<'_, SnapshotInfo>;
}

/// In-memory provider for daemon wiring and contract tests.
#[derive(Clone, Default)]
pub struct InMemorySandboxProvider {
    state: Arc<Mutex<InMemoryState>>,
}

#[derive(Default)]
struct InMemoryState {
    sandboxes: BTreeMap<SandboxId, SandboxRecord>,
}

struct SandboxRecord {
    info: SandboxInfo,
    commands: BTreeMap<CommandId, CommandInfo>,
    files: BTreeMap<String, FileWrite>,
    snapshots: BTreeMap<SnapshotId, SnapshotInfo>,
}

impl InMemorySandboxProvider {
    /// Create an empty in-memory provider.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_state<T>(
        &self,
        operation: impl FnOnce(&mut InMemoryState) -> SandboxResult<T>,
    ) -> SandboxResult<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SandboxError::provider("in-memory provider lock poisoned"))?;
        operation(&mut state)
    }
}

impl SandboxProvider for InMemorySandboxProvider {
    fn capabilities(&self) -> ProviderFuture<'_, SandboxCapabilities> {
        provider_future(Ok(in_memory_capabilities()))
    }

    fn create_sandbox(&self, request: CreateSandboxRequest) -> ProviderFuture<'_, SandboxInfo> {
        let result = self.with_state(|state| {
            let sandbox_id = request.id.unwrap_or_default();
            if state.sandboxes.contains_key(&sandbox_id) {
                return Err(SandboxError::conflict(format!(
                    "sandbox already exists: {sandbox_id}"
                )));
            }

            let now = now_unix_ms();
            let routes = request
                .routes
                .into_iter()
                .map(route_from_create_request)
                .collect();
            let info = SandboxInfo {
                id: sandbox_id,
                status: SandboxStatus::Running,
                architecture: request.architecture.unwrap_or(SandboxArchitecture::Arm64),
                routes,
                created_at_unix_ms: now,
                expires_at_unix_ms: request
                    .timeout_ms
                    .map(|timeout| now.saturating_add(timeout)),
            };
            state.sandboxes.insert(
                sandbox_id,
                SandboxRecord {
                    info: info.clone(),
                    commands: BTreeMap::new(),
                    files: BTreeMap::new(),
                    snapshots: BTreeMap::new(),
                },
            );
            Ok(info)
        });
        provider_future(result)
    }

    fn get_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
        let result = self.with_state(|state| {
            state
                .sandboxes
                .get(&sandbox_id)
                .map(|record| record.info.clone())
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })
        });
        provider_future(result)
    }

    fn delete_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
        let result = self.with_state(|state| {
            let mut record = state
                .sandboxes
                .remove(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            record.info.status = SandboxStatus::Stopped;
            Ok(record.info)
        });
        provider_future(result)
    }

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
    ) -> ProviderFuture<'_, CommandInfo> {
        let result = self.with_state(|state| {
            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            if request.argv.is_empty() {
                return Err(SandboxError::invalid_request(
                    "command argv must not be empty",
                ));
            }

            let now = now_unix_ms();
            let command_id = CommandId::new();
            let (status, finished_at_unix_ms, exit_code) = if request.detached {
                (CommandStatus::Running, None, None)
            } else {
                (CommandStatus::Exited, Some(now), Some(0))
            };
            let command = CommandInfo {
                id: command_id,
                sandbox_id,
                status,
                argv: request.argv,
                cwd: request.cwd,
                started_at_unix_ms: Some(now),
                finished_at_unix_ms,
                exit_code,
                output: CommandOutput::default(),
            };
            record.commands.insert(command_id, command.clone());
            Ok(command)
        });
        provider_future(result)
    }

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        _wait: CommandWait,
    ) -> ProviderFuture<'_, CommandInfo> {
        let result = self.with_state(|state| {
            let record = state
                .sandboxes
                .get(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            record
                .commands
                .get(&command_id)
                .cloned()
                .ok_or(SandboxError::CommandNotFound {
                    sandbox_id,
                    command_id,
                })
        });
        provider_future(result)
    }

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
    ) -> ProviderFuture<'_, CommandInfo> {
        let result = self.with_state(|state| {
            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            let command =
                record
                    .commands
                    .get_mut(&command_id)
                    .ok_or(SandboxError::CommandNotFound {
                        sandbox_id,
                        command_id,
                    })?;
            command.status = CommandStatus::Cancelled;
            command.finished_at_unix_ms = Some(now_unix_ms());
            command.exit_code = None;
            Ok(command.clone())
        });
        provider_future(result)
    }

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> ProviderFuture<'_, WriteFilesResponse> {
        let result = self.with_state(|state| {
            if request.files.len() > MAX_FILES_PER_WRITE {
                return Err(SandboxError::invalid_request(format!(
                    "file write request exceeds {MAX_FILES_PER_WRITE} files"
                )));
            }

            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            let mut paths = Vec::new();
            for file in request.files {
                paths.push(file.path.clone());
                record.files.insert(file.path.clone(), file);
            }
            Ok(WriteFilesResponse { paths })
        });
        provider_future(result)
    }

    fn extend_timeout(
        &self,
        sandbox_id: SandboxId,
        request: ExtendSandboxTimeoutRequest,
    ) -> ProviderFuture<'_, SandboxTimeoutInfo> {
        let result = self.with_state(|state| {
            if request.additional_ms == 0 {
                return Err(SandboxError::invalid_request(
                    "additional_ms must be greater than zero",
                ));
            }

            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            let now = now_unix_ms();
            let base = record.info.expires_at_unix_ms.unwrap_or(now).max(now);
            let expires_at_unix_ms = base.saturating_add(request.additional_ms);
            record.info.expires_at_unix_ms = Some(expires_at_unix_ms);
            Ok(SandboxTimeoutInfo { expires_at_unix_ms })
        });
        provider_future(result)
    }

    fn set_network_policy(
        &self,
        sandbox_id: SandboxId,
        _request: SetNetworkPolicyRequest,
    ) -> ProviderFuture<'_, ()> {
        let result = self.with_state(|state| {
            if !state.sandboxes.contains_key(&sandbox_id) {
                return Err(SandboxError::SandboxNotFound { sandbox_id });
            }

            Err(SandboxError::unsupported("network_policy"))
        });
        provider_future(result)
    }

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
    ) -> ProviderFuture<'_, SnapshotInfo> {
        let result = self.with_state(|state| {
            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            let snapshot_id = SnapshotId::new();
            let snapshot = SnapshotInfo {
                id: snapshot_id,
                sandbox_id,
                status: SnapshotStatus::Ready,
                created_at_unix_ms: now_unix_ms(),
                label: request.label,
            };
            record.snapshots.insert(snapshot_id, snapshot.clone());
            Ok(snapshot)
        });
        provider_future(result)
    }
}

fn route_from_create_request(request: CreateSandboxRouteRequest) -> SandboxRoute {
    let scheme = match request.protocol {
        crate::SandboxRouteProtocol::Http => "http",
        crate::SandboxRouteProtocol::Https => "https",
        crate::SandboxRouteProtocol::Tcp => "tcp",
    };

    SandboxRoute {
        name: request.name,
        protocol: request.protocol,
        target_port: request.target_port,
        url: format!("{scheme}://127.0.0.1:{}", request.target_port),
    }
}

fn provider_future<T: Send + 'static>(result: SandboxResult<T>) -> ProviderFuture<'static, T> {
    Box::pin(async move { result })
}

fn in_memory_capabilities() -> SandboxCapabilities {
    SandboxCapabilities {
        provider: "in_memory".to_string(),
        architectures: vec![SandboxArchitecture::Arm64, SandboxArchitecture::X86_64],
        features: vec![
            SandboxFeature::Commands,
            SandboxFeature::Files,
            SandboxFeature::Routes,
            SandboxFeature::TimeoutExtension,
            SandboxFeature::Snapshots,
            SandboxFeature::UserlandSpawning,
        ],
    }
}

fn now_unix_ms() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_provider_tracks_sandbox_lifecycle() {
        let provider = InMemorySandboxProvider::new();
        let created = provider
            .create_sandbox(CreateSandboxRequest {
                routes: vec![CreateSandboxRouteRequest {
                    name: "preview".to_string(),
                    protocol: crate::SandboxRouteProtocol::Http,
                    target_port: 3000,
                }],
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        assert_eq!(created.status, SandboxStatus::Running);
        assert_eq!(created.routes.len(), 1);
        assert_eq!(created.routes[0].target_port, 3000);
        assert_eq!(created.routes[0].url, "http://127.0.0.1:3000");
        assert_eq!(
            provider.get_sandbox(created.id).await.unwrap().id,
            created.id
        );

        let deleted = provider.delete_sandbox(created.id).await.unwrap();
        assert_eq!(deleted.status, SandboxStatus::Stopped);
        assert!(matches!(
            provider.get_sandbox(created.id).await,
            Err(SandboxError::SandboxNotFound { sandbox_id }) if sandbox_id == created.id
        ));
    }

    #[tokio::test]
    async fn in_memory_provider_uses_route_protocol_in_urls() {
        let provider = InMemorySandboxProvider::new();
        let created = provider
            .create_sandbox(CreateSandboxRequest {
                routes: vec![
                    CreateSandboxRouteRequest {
                        name: "secure".to_string(),
                        protocol: crate::SandboxRouteProtocol::Https,
                        target_port: 3443,
                    },
                    CreateSandboxRouteRequest {
                        name: "database".to_string(),
                        protocol: crate::SandboxRouteProtocol::Tcp,
                        target_port: 5432,
                    },
                ],
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        assert_eq!(created.routes[0].url, "https://127.0.0.1:3443");
        assert_eq!(created.routes[1].url, "tcp://127.0.0.1:5432");
    }

    #[tokio::test]
    async fn in_memory_provider_tracks_commands() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let command = provider
            .create_command(
                sandbox.id,
                CreateCommandRequest {
                    argv: vec!["echo".to_string(), "hello".to_string()],
                    ..CreateCommandRequest::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(command.status, CommandStatus::Exited);
        assert_eq!(command.exit_code, Some(0));

        let fetched = provider
            .get_command(sandbox.id, command.id, CommandWait::default())
            .await
            .unwrap();
        assert_eq!(fetched.argv, vec!["echo", "hello"]);

        let cancelled = provider
            .cancel_command(sandbox.id, command.id)
            .await
            .unwrap();
        assert_eq!(cancelled.status, CommandStatus::Cancelled);
    }

    #[tokio::test]
    async fn in_memory_provider_tracks_detached_commands_as_running() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let command = provider
            .create_command(
                sandbox.id,
                CreateCommandRequest {
                    argv: vec!["pnpm".to_string(), "dev".to_string()],
                    detached: true,
                    ..CreateCommandRequest::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(command.status, CommandStatus::Running);
        assert_eq!(command.exit_code, None);
        assert_eq!(command.finished_at_unix_ms, None);

        let cancelled = provider
            .cancel_command(sandbox.id, command.id)
            .await
            .unwrap();
        assert_eq!(cancelled.status, CommandStatus::Cancelled);
        assert!(cancelled.finished_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn in_memory_provider_tracks_file_writes() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let response = provider
            .write_files(
                sandbox.id,
                WriteFilesRequest {
                    files: vec![FileWrite {
                        path: "/workspace/README.md".to_string(),
                        contents_base64: "SGVsbG8=".to_string(),
                        mode: None,
                    }],
                },
            )
            .await
            .unwrap();

        assert_eq!(response.paths, vec!["/workspace/README.md"]);
        let stored = provider
            .with_state(|state| {
                Ok(state
                    .sandboxes
                    .get(&sandbox.id)
                    .unwrap()
                    .files
                    .get("/workspace/README.md")
                    .unwrap()
                    .contents_base64
                    .clone())
            })
            .unwrap();
        assert_eq!(stored, "SGVsbG8=");
    }

    #[tokio::test]
    async fn in_memory_provider_rejects_too_many_files() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let result = provider
            .write_files(
                sandbox.id,
                WriteFilesRequest {
                    files: (0..=MAX_FILES_PER_WRITE)
                        .map(|index| FileWrite {
                            path: format!("/workspace/{index}.txt"),
                            contents_base64: String::new(),
                            mode: None,
                        })
                        .collect(),
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(SandboxError::InvalidRequest { message })
                if message.contains("file write request exceeds")
        ));
    }

    #[tokio::test]
    async fn in_memory_provider_extends_timeouts_and_creates_snapshots() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let timeout = provider
            .extend_timeout(
                sandbox.id,
                ExtendSandboxTimeoutRequest {
                    additional_ms: 5_000,
                },
            )
            .await
            .unwrap();
        assert!(timeout.expires_at_unix_ms >= sandbox.created_at_unix_ms + 5_000);

        let snapshot = provider
            .create_snapshot(
                sandbox.id,
                CreateSnapshotRequest {
                    label: Some("before-install".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SnapshotStatus::Ready);
        assert_eq!(snapshot.label.as_deref(), Some("before-install"));
    }

    #[tokio::test]
    async fn in_memory_provider_reports_network_policy_as_unsupported() {
        let provider = InMemorySandboxProvider::new();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        assert!(matches!(
            provider
                .set_network_policy(
                    sandbox.id,
                    SetNetworkPolicyRequest {
                        mode: crate::NetworkPolicyMode::DenyAll,
                        allowed_hosts: Vec::new(),
                    },
                )
                .await,
            Err(SandboxError::Unsupported { operation }) if operation == "network_policy"
        ));
    }
}
