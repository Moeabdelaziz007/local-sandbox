//! vfkit-backed sandbox provider.
//!
//! The crate keeps vfkit-specific lifecycle assumptions behind disk, process,
//! route, and guest-agent seams so another crate can implement a Linux provider
//! against the same core contract later.

mod agent;
mod config;
mod disk;
mod process;
mod routes;
mod state;
mod time;

pub use config::VfkitConfig;

use crate::{
    agent::{DefaultAgent, SandboxAgent},
    disk::{DiskHandle, DiskManager, FilesystemDiskManager},
    process::{NoopProcessSpawner, ProcessHandle, ProcessSpawner, ProcessSpec},
    routes::{LocalRouteAllocator, RouteAllocator},
    state::{SandboxRecord, VfkitState},
    time::{Clock, SystemClock},
};
use local_sandbox_core::{
    CommandId, CommandInfo, CommandWait, CreateCommandRequest, CreateSandboxRequest,
    CreateSnapshotRequest, ExtendSandboxTimeoutRequest, ProviderFuture, SandboxArchitecture,
    SandboxCapabilities, SandboxError, SandboxFeature, SandboxId, SandboxInfo, SandboxProvider,
    SandboxResult, SandboxRoute, SandboxStatus, SandboxTimeoutInfo, SetNetworkPolicyRequest,
    SnapshotInfo, WriteFilesRequest, WriteFilesResponse,
};
use std::sync::{Arc, Mutex};

/// vfkit implementation of the product-neutral sandbox provider contract.
#[derive(Clone)]
pub struct VfkitProvider {
    config: VfkitConfig,
    state: Arc<Mutex<VfkitState>>,
    disk: Arc<dyn DiskManager>,
    process: Arc<dyn ProcessSpawner>,
    agent: Arc<dyn SandboxAgent>,
    routes: Arc<dyn RouteAllocator>,
    clock: Arc<dyn Clock>,
}

struct CreateSelection {
    architecture: SandboxArchitecture,
    runtime: String,
}

struct ProviderComponents {
    disk: Arc<dyn DiskManager>,
    process: Arc<dyn ProcessSpawner>,
    agent: Arc<dyn SandboxAgent>,
    routes: Arc<dyn RouteAllocator>,
    clock: Arc<dyn Clock>,
}

impl Default for ProviderComponents {
    fn default() -> Self {
        let config = VfkitConfig::default();
        Self::from_config(&config)
    }
}

impl ProviderComponents {
    fn from_config(config: &VfkitConfig) -> Self {
        Self {
            disk: Arc::new(FilesystemDiskManager::new(config)),
            process: Arc::new(NoopProcessSpawner),
            agent: Arc::new(DefaultAgent::default()),
            routes: Arc::new(LocalRouteAllocator::new(config.route_host.clone())),
            clock: Arc::new(SystemClock),
        }
    }
}

impl Default for VfkitProvider {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl VfkitProvider {
    /// Create a provider instance from explicit vfkit configuration.
    #[must_use]
    pub fn new(config: VfkitConfig) -> Self {
        let components = ProviderComponents::from_config(&config);
        Self::with_components(config, components)
    }

    /// Create a provider instance using default vfkit configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(VfkitConfig::default())
    }

    /// Return the provider configuration.
    #[must_use]
    pub const fn config(&self) -> &VfkitConfig {
        &self.config
    }

    /// Create a sandbox through the vfkit lifecycle model.
    pub fn create(&self, request: &CreateSandboxRequest) -> SandboxResult<SandboxInfo> {
        self.create_sandbox_sync(request)
    }

    fn with_components(config: VfkitConfig, components: ProviderComponents) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(VfkitState::default())),
            disk: components.disk,
            process: components.process,
            agent: components.agent,
            routes: components.routes,
            clock: components.clock,
        }
    }

    fn create_sandbox_sync(&self, request: &CreateSandboxRequest) -> SandboxResult<SandboxInfo> {
        let selection = self.validate_create_request(request)?;
        let sandbox_id = request.id.unwrap_or_default();
        let now = self.clock.now_unix_ms();
        let created_info = SandboxInfo {
            id: sandbox_id,
            status: SandboxStatus::Created,
            architecture: selection.architecture,
            routes: Vec::new(),
            created_at_unix_ms: now,
            expires_at_unix_ms: request
                .timeout_ms
                .map(|timeout| now.saturating_add(timeout)),
        };
        self.reserve_sandbox(created_info)?;

        let routes = match self.routes.allocate(sandbox_id, &request.routes) {
            Ok(routes) => routes,
            Err(error) => {
                self.remove_reservation(sandbox_id);
                return Err(error);
            }
        };

        let disk = match self.disk.prepare(sandbox_id) {
            Ok(disk) => disk,
            Err(error) => {
                self.cleanup_after_failed_create(sandbox_id, None, None, &routes);
                return Err(error);
            }
        };

        let process_spec = ProcessSpec {
            sandbox_id,
            architecture: selection.architecture,
            runtime: selection.runtime,
            disk_path: disk.path.as_path().to_path_buf(),
            snapshot_id: request.snapshot_id,
            resources: request.resources,
        };
        let process = match self.process.start(&process_spec) {
            Ok(process) => process,
            Err(error) => {
                self.cleanup_after_failed_create(sandbox_id, Some(&disk), None, &routes);
                return Err(error);
            }
        };

        let register_agent = process.agent_socket_path.clone().map_or_else(
            || self.agent.register(sandbox_id),
            |socket_path| self.agent.register_protocol_client(sandbox_id, socket_path),
        );
        if let Err(error) = register_agent {
            self.cleanup_after_failed_create(sandbox_id, Some(&disk), Some(&process), &routes);
            return Err(error);
        }

        let running_info = SandboxInfo {
            id: sandbox_id,
            status: SandboxStatus::Running,
            architecture: selection.architecture,
            routes: routes.clone(),
            created_at_unix_ms: now,
            expires_at_unix_ms: request
                .timeout_ms
                .map(|timeout| now.saturating_add(timeout)),
        };
        if let Err(error) =
            self.mark_sandbox_running(running_info.clone(), disk.clone(), process.clone())
        {
            self.cleanup_after_failed_create(sandbox_id, Some(&disk), Some(&process), &routes);
            return Err(error);
        }
        Ok(running_info)
    }

    fn get_sandbox_sync(&self, sandbox_id: SandboxId) -> SandboxResult<SandboxInfo> {
        let state = self.lock_state()?;
        state
            .sandboxes
            .get(&sandbox_id)
            .map(|record| record.info.clone())
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })
    }

    fn delete_sandbox_sync(&self, sandbox_id: SandboxId) -> SandboxResult<SandboxInfo> {
        let (mut info, disk, process, routes) = {
            let mut state = self.lock_state()?;
            let record = state
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
            record.info.status = SandboxStatus::Stopping;
            (
                record.info.clone(),
                record.disk.clone(),
                record.process.clone(),
                record.info.routes.clone(),
            )
        };

        let stop_result = process
            .as_ref()
            .map_or(Ok(()), |handle| self.process.stop(handle));
        let route_result = self.routes.release(sandbox_id, &routes);
        let disk_result = disk
            .as_ref()
            .map_or(Ok(()), |handle| self.disk.cleanup(handle));
        let agent_result = self.agent.unregister(sandbox_id);

        first_cleanup_error([stop_result, route_result, disk_result, agent_result])?;

        {
            let mut state = self.lock_state()?;
            state.sandboxes.remove(&sandbox_id);
        }

        info.status = SandboxStatus::Stopped;
        Ok(info)
    }

    fn extend_timeout_sync(
        &self,
        sandbox_id: SandboxId,
        request: ExtendSandboxTimeoutRequest,
    ) -> SandboxResult<SandboxTimeoutInfo> {
        if request.additional_ms == 0 {
            return Err(SandboxError::invalid_request(
                "additional_ms must be greater than zero",
            ));
        }

        let now = self.clock.now_unix_ms();
        let mut state = self.lock_state()?;
        let record = state
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        let base = record.info.expires_at_unix_ms.unwrap_or(now).max(now);
        let expires_at_unix_ms = base.saturating_add(request.additional_ms);
        record.info.expires_at_unix_ms = Some(expires_at_unix_ms);
        Ok(SandboxTimeoutInfo { expires_at_unix_ms })
    }

    fn ensure_running(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        let state = self.lock_state()?;
        let record = state
            .sandboxes
            .get(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        if record.info.status == SandboxStatus::Running {
            Ok(())
        } else {
            Err(SandboxError::invalid_request(format!(
                "sandbox is not running: {sandbox_id}"
            )))
        }
    }

    fn validate_create_request(
        &self,
        request: &CreateSandboxRequest,
    ) -> SandboxResult<CreateSelection> {
        let architecture = request.architecture.unwrap_or_else(|| {
            self.config
                .supported_architectures
                .first()
                .copied()
                .unwrap_or(SandboxArchitecture::Arm64)
        });
        if !self.config.supported_architectures.contains(&architecture) {
            return Err(SandboxError::unsupported("architecture"));
        }

        if request.userland_spawning.unwrap_or(false) {
            return Err(SandboxError::unsupported("userland_spawning"));
        }

        let runtime = request
            .runtime
            .clone()
            .unwrap_or_else(|| self.config.default_runtime.clone());
        if !self.config.supported_runtimes.contains(&runtime) {
            return Err(SandboxError::unsupported("runtime"));
        }

        Ok(CreateSelection {
            architecture,
            runtime,
        })
    }

    fn reserve_sandbox(&self, info: SandboxInfo) -> SandboxResult<()> {
        let mut state = self.lock_state()?;
        if state.sandboxes.contains_key(&info.id) {
            return Err(SandboxError::conflict(format!(
                "sandbox already exists: {}",
                info.id
            )));
        }
        state.sandboxes.insert(info.id, SandboxRecord::new(info));
        Ok(())
    }

    fn mark_sandbox_running(
        &self,
        info: SandboxInfo,
        disk: DiskHandle,
        process: ProcessHandle,
    ) -> SandboxResult<()> {
        let mut state = self.lock_state()?;
        let record = state
            .sandboxes
            .get_mut(&info.id)
            .ok_or_else(|| SandboxError::provider("sandbox reservation disappeared"))?;
        record.info = info;
        record.disk = Some(disk);
        record.process = Some(process);
        Ok(())
    }

    fn cleanup_after_failed_create(
        &self,
        sandbox_id: SandboxId,
        disk: Option<&DiskHandle>,
        process: Option<&ProcessHandle>,
        routes: &[SandboxRoute],
    ) {
        if let Some(process) = process {
            let _ = self.process.stop(process);
        }
        let _ = self.routes.release(sandbox_id, routes);
        if let Some(disk) = disk {
            let _ = self.disk.cleanup(disk);
        }
        let _ = self.agent.unregister(sandbox_id);
        self.remove_reservation(sandbox_id);
    }

    fn remove_reservation(&self, sandbox_id: SandboxId) {
        if let Ok(mut state) = self.lock_state() {
            state.sandboxes.remove(&sandbox_id);
        }
    }

    fn lock_state(&self) -> SandboxResult<std::sync::MutexGuard<'_, VfkitState>> {
        self.state
            .lock()
            .map_err(|_| SandboxError::provider("vfkit provider state lock poisoned"))
    }
}

fn first_cleanup_error(results: [SandboxResult<()>; 4]) -> SandboxResult<()> {
    results
        .into_iter()
        .collect::<SandboxResult<Vec<_>>>()
        .map(drop)
}

impl SandboxProvider for VfkitProvider {
    fn capabilities(&self) -> ProviderFuture<'_, SandboxCapabilities> {
        provider_future(Ok(SandboxCapabilities {
            provider: "vfkit".to_string(),
            architectures: self.config.supported_architectures.clone(),
            features: vec![
                SandboxFeature::Commands,
                SandboxFeature::Files,
                SandboxFeature::Routes,
                SandboxFeature::TimeoutExtension,
                SandboxFeature::Snapshots,
            ],
        }))
    }

    fn create_sandbox(&self, request: CreateSandboxRequest) -> ProviderFuture<'_, SandboxInfo> {
        let provider = self.clone();
        blocking_provider_future(move || provider.create_sandbox_sync(&request))
    }

    fn get_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
        provider_future(self.get_sandbox_sync(sandbox_id))
    }

    fn delete_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
        let provider = self.clone();
        blocking_provider_future(move || provider.delete_sandbox_sync(sandbox_id))
    }

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
    ) -> ProviderFuture<'_, CommandInfo> {
        let provider = self.clone();
        blocking_provider_future(move || {
            provider.ensure_running(sandbox_id).and_then(|()| {
                provider
                    .agent
                    .create_command(sandbox_id, request, provider.clock.now_unix_ms())
            })
        })
    }

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        wait: CommandWait,
    ) -> ProviderFuture<'_, CommandInfo> {
        let provider = self.clone();
        blocking_provider_future(move || {
            provider.ensure_running(sandbox_id).and_then(|()| {
                provider.agent.get_command(
                    sandbox_id,
                    command_id,
                    wait,
                    provider.clock.now_unix_ms(),
                )
            })
        })
    }

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
    ) -> ProviderFuture<'_, CommandInfo> {
        let provider = self.clone();
        blocking_provider_future(move || {
            provider.ensure_running(sandbox_id).and_then(|()| {
                provider
                    .agent
                    .cancel_command(sandbox_id, command_id, provider.clock.now_unix_ms())
            })
        })
    }

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> ProviderFuture<'_, WriteFilesResponse> {
        let provider = self.clone();
        blocking_provider_future(move || {
            provider
                .ensure_running(sandbox_id)
                .and_then(|()| provider.agent.write_files(sandbox_id, request))
        })
    }

    fn extend_timeout(
        &self,
        sandbox_id: SandboxId,
        request: ExtendSandboxTimeoutRequest,
    ) -> ProviderFuture<'_, SandboxTimeoutInfo> {
        provider_future(self.extend_timeout_sync(sandbox_id, request))
    }

    fn set_network_policy(
        &self,
        sandbox_id: SandboxId,
        _request: SetNetworkPolicyRequest,
    ) -> ProviderFuture<'_, ()> {
        let result = self
            .ensure_running(sandbox_id)
            .and_then(|()| Err(SandboxError::unsupported("network_policy")));
        provider_future(result)
    }

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
    ) -> ProviderFuture<'_, SnapshotInfo> {
        let provider = self.clone();
        blocking_provider_future(move || {
            provider.ensure_running(sandbox_id).and_then(|()| {
                provider
                    .agent
                    .create_snapshot(sandbox_id, request, provider.clock.now_unix_ms())
            })
        })
    }
}

fn provider_future<T: Send + 'static>(result: SandboxResult<T>) -> ProviderFuture<'static, T> {
    Box::pin(async move { result })
}

fn blocking_provider_future<T: Send + 'static>(
    operation: impl FnOnce() -> SandboxResult<T> + Send + 'static,
) -> ProviderFuture<'static, T> {
    Box::pin(async move {
        tokio::task::spawn_blocking(operation)
            .await
            .map_err(|error| SandboxError::provider(format!("provider task failed: {error}")))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::InMemoryAgent,
        disk::{DiskHandle, SandboxDiskPath},
        routes::protocol_scheme,
        time::Clock,
    };
    use local_sandbox_core::{
        CreateSandboxRouteRequest, FileWrite, NetworkPolicyMode, SandboxErrorCode,
        SandboxRouteProtocol, SnapshotStatus,
    };
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    #[tokio::test]
    async fn vfkit_capabilities_omit_unsupported_features() {
        let (provider, _fakes) = test_provider();

        let capabilities = provider.capabilities().await.unwrap();

        assert_eq!(capabilities.provider, "vfkit");
        assert_eq!(capabilities.architectures, vec![SandboxArchitecture::Arm64]);
        assert!(capabilities.features.contains(&SandboxFeature::Commands));
        assert!(capabilities.features.contains(&SandboxFeature::Files));
        assert!(capabilities.features.contains(&SandboxFeature::Routes));
        assert!(capabilities.features.contains(&SandboxFeature::Snapshots));
        assert!(!capabilities
            .features
            .contains(&SandboxFeature::NetworkPolicy));
        assert!(!capabilities
            .features
            .contains(&SandboxFeature::UserlandSpawning));
    }

    #[tokio::test]
    async fn create_success_records_running_sandbox_routes_and_expiry() {
        let (provider, fakes) = test_provider();
        let sandbox_id = SandboxId::new();

        let created = provider
            .create_sandbox(CreateSandboxRequest {
                id: Some(sandbox_id),
                routes: vec![CreateSandboxRouteRequest {
                    name: "preview".to_string(),
                    protocol: SandboxRouteProtocol::Http,
                    target_port: 3000,
                }],
                timeout_ms: Some(500),
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        assert_eq!(created.id, sandbox_id);
        assert_eq!(created.status, SandboxStatus::Running);
        assert_eq!(created.created_at_unix_ms, 1_000);
        assert_eq!(created.expires_at_unix_ms, Some(1_500));
        assert_eq!(created.routes.len(), 1);
        assert_eq!(created.routes[0].url, "http://127.0.0.1:3000");
        assert_eq!(provider.get_sandbox(sandbox_id).await.unwrap(), created);
        assert_eq!(fakes.disk.prepared_ids(), vec![sandbox_id]);
        assert_eq!(fakes.process.started_ids(), vec![sandbox_id]);
        assert_eq!(fakes.routes.allocated_ids(), vec![sandbox_id]);
    }

    #[tokio::test]
    async fn duplicate_sandbox_id_conflicts_without_new_side_effects() {
        let (provider, fakes) = test_provider();
        let sandbox_id = SandboxId::new();
        provider
            .create_sandbox(CreateSandboxRequest {
                id: Some(sandbox_id),
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        let result = provider
            .create_sandbox(CreateSandboxRequest {
                id: Some(sandbox_id),
                ..CreateSandboxRequest::default()
            })
            .await;

        assert!(matches!(
            result,
            Err(SandboxError::Conflict { message }) if message.contains("sandbox already exists")
        ));
        assert_eq!(fakes.disk.prepared_ids(), vec![sandbox_id]);
        assert_eq!(fakes.process.started_ids(), vec![sandbox_id]);
        assert_eq!(fakes.routes.allocated_ids(), vec![sandbox_id]);
    }

    #[tokio::test]
    async fn unsupported_create_options_fail_before_side_effects() {
        assert_unsupported_create_fails_before_side_effects(CreateSandboxRequest {
            architecture: Some(SandboxArchitecture::X86_64),
            ..CreateSandboxRequest::default()
        })
        .await;
        assert_unsupported_create_fails_before_side_effects(CreateSandboxRequest {
            runtime: Some("linux-default".to_string()),
            ..CreateSandboxRequest::default()
        })
        .await;
        assert_unsupported_create_fails_before_side_effects(CreateSandboxRequest {
            userland_spawning: Some(true),
            ..CreateSandboxRequest::default()
        })
        .await;
    }

    #[tokio::test]
    async fn delete_stops_process_releases_routes_and_cleans_disk() {
        let (provider, fakes) = test_provider();
        let created = provider
            .create_sandbox(CreateSandboxRequest {
                routes: vec![CreateSandboxRouteRequest {
                    name: "preview".to_string(),
                    protocol: SandboxRouteProtocol::Https,
                    target_port: 3443,
                }],
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        let deleted = provider.delete_sandbox(created.id).await.unwrap();

        assert_eq!(deleted.status, SandboxStatus::Stopped);
        assert_eq!(fakes.process.stopped_ids(), vec![created.id]);
        assert_eq!(fakes.disk.cleaned_ids(), vec![created.id]);
        assert_eq!(fakes.routes.released_ids(), vec![created.id]);
        assert!(matches!(
            provider.get_sandbox(created.id).await,
            Err(SandboxError::SandboxNotFound { sandbox_id }) if sandbox_id == created.id
        ));
    }

    #[tokio::test]
    async fn delete_keeps_sandbox_tracked_when_cleanup_fails() {
        let (provider, fakes) = test_provider();
        let created = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();
        fakes.disk.fail_cleanup();

        let result = provider.delete_sandbox(created.id).await;

        assert!(matches!(
            result,
            Err(SandboxError::Provider { message }) if message == "disk cleanup failed"
        ));
        assert_eq!(fakes.process.stopped_ids(), vec![created.id]);
        assert_eq!(fakes.disk.cleaned_ids(), vec![created.id]);
        assert!(matches!(
            provider.get_sandbox(created.id).await,
            Ok(info) if info.status == SandboxStatus::Stopping
        ));
    }

    #[tokio::test]
    async fn create_failure_cleans_up_allocated_resources() {
        let (provider, fakes) = test_provider();
        fakes.process.fail_start();
        let sandbox_id = SandboxId::new();

        let result = provider
            .create_sandbox(CreateSandboxRequest {
                id: Some(sandbox_id),
                routes: vec![CreateSandboxRouteRequest {
                    name: "preview".to_string(),
                    protocol: SandboxRouteProtocol::Tcp,
                    target_port: 5432,
                }],
                ..CreateSandboxRequest::default()
            })
            .await;

        assert!(
            matches!(result, Err(SandboxError::Provider { message }) if message == "process start failed")
        );
        assert_eq!(fakes.disk.prepared_ids(), vec![sandbox_id]);
        assert_eq!(fakes.disk.cleaned_ids(), vec![sandbox_id]);
        assert_eq!(fakes.routes.allocated_ids(), vec![sandbox_id]);
        assert_eq!(fakes.routes.released_ids(), vec![sandbox_id]);
        assert!(fakes.process.stopped_ids().is_empty());
        assert!(matches!(
            provider.get_sandbox(sandbox_id).await,
            Err(SandboxError::SandboxNotFound { sandbox_id: missing }) if missing == sandbox_id
        ));
    }

    #[tokio::test]
    async fn agent_backed_command_files_and_snapshots_work() {
        let (provider, _fakes) = test_provider();
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
        assert_eq!(command.status, local_sandbox_core::CommandStatus::Exited);
        assert_eq!(
            provider
                .get_command(sandbox.id, command.id, CommandWait::default())
                .await
                .unwrap()
                .argv,
            vec!["echo", "hello"]
        );

        let write_response = provider
            .write_files(
                sandbox.id,
                WriteFilesRequest {
                    files: vec![FileWrite {
                        path: "/workspace/index.ts".to_string(),
                        contents_base64: "Y29uc29sZS5sb2coJ2hpJyk=".to_string(),
                        mode: None,
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(write_response.paths, vec!["/workspace/index.ts"]);

        let snapshot = provider
            .create_snapshot(
                sandbox.id,
                CreateSnapshotRequest {
                    label: Some("checkpoint".to_string()),
                },
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SnapshotStatus::Ready);
        assert_eq!(snapshot.label.as_deref(), Some("checkpoint"));
    }

    #[tokio::test]
    async fn extend_timeout_rejects_zero_and_extends_from_now_when_expired() {
        let (provider, _fakes) = test_provider();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest {
                timeout_ms: Some(1),
                ..CreateSandboxRequest::default()
            })
            .await
            .unwrap();

        assert!(matches!(
            provider
                .extend_timeout(
                    sandbox.id,
                    ExtendSandboxTimeoutRequest { additional_ms: 0 },
                )
                .await,
            Err(SandboxError::InvalidRequest { message })
                if message == "additional_ms must be greater than zero"
        ));

        let timeout = provider
            .extend_timeout(
                sandbox.id,
                ExtendSandboxTimeoutRequest { additional_ms: 500 },
            )
            .await
            .unwrap();

        assert_eq!(timeout.expires_at_unix_ms, 1_501);
    }

    #[tokio::test]
    async fn network_policy_is_typed_unsupported() {
        let (provider, _fakes) = test_provider();
        let sandbox = provider
            .create_sandbox(CreateSandboxRequest::default())
            .await
            .unwrap();

        let result = provider
            .set_network_policy(
                sandbox.id,
                SetNetworkPolicyRequest {
                    mode: NetworkPolicyMode::DenyAll,
                    allowed_hosts: Vec::new(),
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(error) if error.code() == SandboxErrorCode::Unsupported
        ));
    }

    async fn assert_unsupported_create_fails_before_side_effects(request: CreateSandboxRequest) {
        let sandbox_id = SandboxId::new();
        let (provider, fakes) = test_provider();
        let result = provider
            .create_sandbox(CreateSandboxRequest {
                id: Some(sandbox_id),
                ..request
            })
            .await;

        assert!(matches!(result, Err(SandboxError::Unsupported { .. })));
        assert!(fakes.disk.prepared_ids().is_empty());
        assert!(fakes.process.started_ids().is_empty());
        assert!(fakes.routes.allocated_ids().is_empty());
        assert!(matches!(
            provider.get_sandbox(sandbox_id).await,
            Err(SandboxError::SandboxNotFound { sandbox_id: missing }) if missing == sandbox_id
        ));
    }

    fn test_provider() -> (VfkitProvider, Fakes) {
        let disk = Arc::new(FakeDisk::default());
        let process = Arc::new(FakeProcess::default());
        let routes = Arc::new(FakeRoutes::default());
        let clock = Arc::new(FixedClock { now: 1_000 });
        let components = ProviderComponents {
            disk: disk.clone(),
            process: process.clone(),
            agent: Arc::new(InMemoryAgent::default()),
            routes: routes.clone(),
            clock,
        };
        let provider = VfkitProvider::with_components(VfkitConfig::default(), components);
        (
            provider,
            Fakes {
                disk,
                process,
                routes,
            },
        )
    }

    struct Fakes {
        disk: Arc<FakeDisk>,
        process: Arc<FakeProcess>,
        routes: Arc<FakeRoutes>,
    }

    struct FixedClock {
        now: u64,
    }

    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            self.now
        }
    }

    #[derive(Default)]
    struct FakeDisk {
        prepared: Mutex<Vec<SandboxId>>,
        cleaned: Mutex<Vec<SandboxId>>,
        fail_cleanup: Mutex<bool>,
    }

    impl FakeDisk {
        fn fail_cleanup(&self) {
            *self.fail_cleanup.lock().unwrap() = true;
        }

        fn prepared_ids(&self) -> Vec<SandboxId> {
            self.prepared.lock().unwrap().clone()
        }

        fn cleaned_ids(&self) -> Vec<SandboxId> {
            self.cleaned.lock().unwrap().clone()
        }
    }

    impl DiskManager for FakeDisk {
        fn prepare(&self, sandbox_id: SandboxId) -> SandboxResult<DiskHandle> {
            self.prepared.lock().unwrap().push(sandbox_id);
            Ok(DiskHandle {
                sandbox_id,
                path: SandboxDiskPath::new(&PathBuf::from("/fake-disk"), &sandbox_id.to_string())?,
            })
        }

        fn cleanup(&self, handle: &DiskHandle) -> SandboxResult<()> {
            self.cleaned.lock().unwrap().push(handle.sandbox_id);
            if *self.fail_cleanup.lock().unwrap() {
                return Err(SandboxError::provider("disk cleanup failed"));
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeProcess {
        started: Mutex<Vec<SandboxId>>,
        stopped: Mutex<Vec<SandboxId>>,
        fail_start: Mutex<bool>,
    }

    impl FakeProcess {
        fn fail_start(&self) {
            *self.fail_start.lock().unwrap() = true;
        }

        fn started_ids(&self) -> Vec<SandboxId> {
            self.started.lock().unwrap().clone()
        }

        fn stopped_ids(&self) -> Vec<SandboxId> {
            self.stopped.lock().unwrap().clone()
        }
    }

    impl ProcessSpawner for FakeProcess {
        fn start(&self, spec: &ProcessSpec) -> SandboxResult<ProcessHandle> {
            if *self.fail_start.lock().unwrap() {
                return Err(SandboxError::provider("process start failed"));
            }
            self.started.lock().unwrap().push(spec.sandbox_id);
            Ok(ProcessHandle {
                sandbox_id: spec.sandbox_id,
                token: format!("fake-process:{}", spec.sandbox_id),
                agent_socket_path: None,
            })
        }

        fn stop(&self, handle: &ProcessHandle) -> SandboxResult<()> {
            self.stopped.lock().unwrap().push(handle.sandbox_id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRoutes {
        allocated: Mutex<Vec<SandboxId>>,
        released: Mutex<Vec<SandboxId>>,
    }

    impl FakeRoutes {
        fn allocated_ids(&self) -> Vec<SandboxId> {
            self.allocated.lock().unwrap().clone()
        }

        fn released_ids(&self) -> Vec<SandboxId> {
            self.released.lock().unwrap().clone()
        }
    }

    impl RouteAllocator for FakeRoutes {
        fn allocate(
            &self,
            sandbox_id: SandboxId,
            requests: &[CreateSandboxRouteRequest],
        ) -> SandboxResult<Vec<SandboxRoute>> {
            self.allocated.lock().unwrap().push(sandbox_id);
            Ok(requests
                .iter()
                .map(|request| SandboxRoute {
                    name: request.name.clone(),
                    protocol: request.protocol,
                    target_port: request.target_port,
                    url: format!(
                        "{}://127.0.0.1:{}",
                        protocol_scheme(request.protocol),
                        request.target_port
                    ),
                })
                .collect())
        }

        fn release(&self, sandbox_id: SandboxId, _routes: &[SandboxRoute]) -> SandboxResult<()> {
            self.released.lock().unwrap().push(sandbox_id);
            Ok(())
        }
    }
}
