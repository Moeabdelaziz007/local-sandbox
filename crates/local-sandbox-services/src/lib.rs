//! Local service orchestration boundary.

use local_sandbox_core::{
    LocalCredentials, LocalEndpoint, LocalEndpointProtocol, LocalEndpointPurpose,
    LocalEndpointVisibility, LocalHealth, LocalHealthStatus, LocalLogProfile,
    LocalObjectStorageCapabilities, LocalObjectStorageProfile, LocalServiceProfile, SandboxError,
    SandboxResult,
};
use s3s::{auth::SimpleAuth, service::S3ServiceBuilder};
use s3s_fs::FileSystem;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    net::IpAddr,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpStream, process::Command, sync::oneshot, task::JoinHandle, time::sleep};

use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::{conn::auto::Builder as ConnBuilder, graceful::GracefulShutdown},
};

const DEFAULT_OBJECT_STORAGE_API_PORT: u16 = 9_000;
const DEFAULT_MINIO_CONSOLE_PORT: u16 = 9_001;
const DEFAULT_ACCESS_KEY_ID: &str = "local-sandbox";
const DEFAULT_SECRET_ACCESS_KEY: &str = "local-sandbox-secret";
const DEFAULT_REGION: &str = "us-east-1";

/// Environment variable for the object-storage backend implementation.
pub const ENV_OBJECT_STORAGE_BACKEND: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_BACKEND";
/// Environment variable for an explicit `MinIO` binary path.
pub const ENV_MINIO_BINARY: &str = "LOCAL_SANDBOX_MINIO_BINARY";
/// Environment variable for object-storage data directory.
pub const ENV_OBJECT_STORAGE_DATA_DIR: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_DATA_DIR";
/// Environment variable for object-storage API bind host.
pub const ENV_OBJECT_STORAGE_API_HOST: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_API_HOST";
/// Environment variable for object-storage API port.
pub const ENV_OBJECT_STORAGE_API_PORT: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_API_PORT";
/// Environment variable for object-storage console port.
pub const ENV_OBJECT_STORAGE_CONSOLE_PORT: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_CONSOLE_PORT";
/// Environment variable for S3-compatible object-storage region.
pub const ENV_OBJECT_STORAGE_REGION: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_REGION";
/// Environment variable for object-storage access key id.
pub const ENV_OBJECT_STORAGE_ACCESS_KEY_ID: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_ACCESS_KEY_ID";
/// Environment variable for object-storage secret access key.
pub const ENV_OBJECT_STORAGE_SECRET_ACCESS_KEY: &str =
    "LOCAL_SANDBOX_OBJECT_STORAGE_SECRET_ACCESS_KEY";
/// Environment variable for the guest-visible object-storage host.
pub const ENV_OBJECT_STORAGE_GUEST_HOST: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_GUEST_HOST";
/// Environment variable for the optional object-storage process log path.
pub const ENV_OBJECT_STORAGE_LOG_PATH: &str = "LOCAL_SANDBOX_OBJECT_STORAGE_LOG_PATH";

/// Local service kind managed by the substrate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalServiceKind {
    /// S3-compatible object storage.
    S3,
    /// `PostgreSQL` database.
    Postgres,
}

/// Object-storage backend implementation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ObjectStorageBackendKind {
    /// Embedded Rust S3-compatible backend.
    #[default]
    Embedded,
    /// External `MinIO` backend, retained as an optional parity/reference path.
    Minio,
}

impl ObjectStorageBackendKind {
    fn from_env_value(value: &str) -> SandboxResult<Self> {
        match value {
            "embedded" => Ok(Self::Embedded),
            "minio" => Ok(Self::Minio),
            _ => Err(SandboxError::invalid_request(format!(
                "{ENV_OBJECT_STORAGE_BACKEND} must be embedded or minio"
            ))),
        }
    }
}

/// Configuration for local S3-compatible object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectStorageConfig {
    /// Backend implementation to use for object storage.
    pub backend: ObjectStorageBackendKind,
    /// Directory used for object-storage data.
    pub data_dir: PathBuf,
    /// Path to the `MinIO` server binary for the optional parity backend.
    pub minio_binary: Option<PathBuf>,
    /// Host interface used by the object-storage API listener.
    pub api_host: String,
    /// Host port used by the object-storage API listener.
    pub api_port: u16,
    /// Host interface used by the optional `MinIO` console listener.
    pub console_host: String,
    /// Host port used by the optional `MinIO` console listener.
    pub console_port: u16,
    /// Host name used by clients in the host process namespace.
    pub host_endpoint_host: String,
    /// Host name used by clients inside sandbox guests.
    pub guest_endpoint_host: String,
    /// Region used by S3-compatible clients.
    pub region: String,
    /// Access key identifier for S3-compatible credentials.
    pub access_key_id: String,
    /// Secret access key for S3-compatible credentials.
    pub secret_access_key: String,
    /// Optional file path where process-backed backends write stdout/stderr.
    pub log_path: Option<PathBuf>,
    /// Startup readiness deadline.
    pub readiness_timeout: Duration,
}

impl Default for ObjectStorageConfig {
    fn default() -> Self {
        let api_host = "127.0.0.1".to_string();
        let api_port = DEFAULT_OBJECT_STORAGE_API_PORT;
        let console_host = "127.0.0.1".to_string();
        let console_port = DEFAULT_MINIO_CONSOLE_PORT;

        Self {
            backend: ObjectStorageBackendKind::Embedded,
            data_dir: env::temp_dir()
                .join("local-sandbox")
                .join("object-storage-data"),
            minio_binary: None,
            api_host,
            api_port,
            console_host,
            console_port,
            host_endpoint_host: "127.0.0.1".to_string(),
            guest_endpoint_host: "host.internal".to_string(),
            region: DEFAULT_REGION.to_string(),
            access_key_id: DEFAULT_ACCESS_KEY_ID.to_string(),
            secret_access_key: DEFAULT_SECRET_ACCESS_KEY.to_string(),
            log_path: Some(
                env::temp_dir()
                    .join("local-sandbox")
                    .join("object-storage.log"),
            ),
            readiness_timeout: Duration::from_secs(30),
        }
    }
}

impl ObjectStorageConfig {
    /// Build object-storage config from product-neutral local-sandbox environment variables.
    pub fn from_env() -> SandboxResult<Self> {
        Self::from_env_reader(|name| env::var_os(name))
    }

    fn from_env_reader(mut read: impl FnMut(&str) -> Option<OsString>) -> SandboxResult<Self> {
        let mut config = Self::default();

        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_BACKEND)? {
            config.backend = ObjectStorageBackendKind::from_env_value(&value)?;
        }
        if let Some(value) = read_env_path(&mut read, ENV_MINIO_BINARY) {
            config.backend = ObjectStorageBackendKind::Minio;
            config.minio_binary = Some(value);
        }
        if let Some(value) = read_env_path(&mut read, ENV_OBJECT_STORAGE_DATA_DIR) {
            config.data_dir = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_API_HOST)? {
            config.host_endpoint_host = host_endpoint_host_for_api_host(&value);
            config.api_host.clone_from(&value);
            config.console_host = value;
        }
        if let Some(value) = read_env_u16(&mut read, ENV_OBJECT_STORAGE_API_PORT)? {
            config.api_port = value;
        }
        if let Some(value) = read_env_u16(&mut read, ENV_OBJECT_STORAGE_CONSOLE_PORT)? {
            config.console_port = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_REGION)? {
            config.region = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_ACCESS_KEY_ID)? {
            config.access_key_id = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_SECRET_ACCESS_KEY)? {
            config.secret_access_key = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_OBJECT_STORAGE_GUEST_HOST)? {
            config.guest_endpoint_host = value;
        }
        if let Some(value) = read_env_path(&mut read, ENV_OBJECT_STORAGE_LOG_PATH) {
            config.log_path = Some(value);
        }

        Ok(config)
    }

    /// Endpoint reachable from the host process namespace.
    #[must_use]
    pub fn host_endpoint(&self) -> String {
        format!("http://{}:{}", self.host_endpoint_host, self.api_port)
    }

    /// Endpoint reachable from sandbox guests.
    #[must_use]
    pub fn guest_endpoint(&self) -> String {
        format!("http://{}:{}", self.guest_endpoint_host, self.api_port)
    }

    /// Hostname to use for local readiness probes.
    #[must_use]
    pub fn probe_host(&self) -> &str {
        if self.api_host == "0.0.0.0" || self.api_host == "::" {
            &self.host_endpoint_host
        } else {
            &self.api_host
        }
    }

    /// Return the product-neutral S3-compatible operation support matrix.
    #[must_use]
    pub const fn capabilities() -> LocalObjectStorageCapabilities {
        LocalObjectStorageCapabilities {
            create_bucket: true,
            put_object: true,
            get_object: true,
            head_object: true,
            delete_object: true,
            delete_objects: true,
            list_objects_v2: true,
            copy_object: true,
            presigned_get: true,
            presigned_put: true,
            cors: false,
            browser_upload: false,
            versioning: false,
            list_object_versions: false,
        }
    }

    /// Build the service profile for this object-storage configuration and health.
    #[must_use]
    pub fn profile(&self, health: &LocalHealth) -> LocalServiceProfile {
        LocalServiceProfile {
            health: health.clone(),
            endpoints: vec![
                endpoint(
                    "host_api",
                    LocalEndpointVisibility::Host,
                    LocalEndpointPurpose::ObjectStorageApi,
                    Some(self.host_endpoint()),
                    health,
                ),
                endpoint(
                    "guest_api",
                    LocalEndpointVisibility::Guest,
                    LocalEndpointPurpose::ObjectStorageApi,
                    Some(self.guest_endpoint()),
                    health,
                ),
            ],
            credentials: Some(LocalCredentials {
                access_key_id: Some(self.access_key_id.clone()),
                secret_access_key: Some(self.secret_access_key.clone()),
                username: None,
                password: None,
                token: None,
            }),
            logs: self
                .log_path
                .as_ref()
                .map(|path| LocalLogProfile {
                    name: "service".to_string(),
                    path: Some(path.display().to_string()),
                    stream: None,
                })
                .into_iter()
                .collect(),
            object_storage: Some(LocalObjectStorageProfile {
                region: self.region.clone(),
                force_path_style: true,
                capabilities: Self::capabilities(),
            }),
        }
    }
}

fn endpoint(
    name: &str,
    visibility: LocalEndpointVisibility,
    purpose: LocalEndpointPurpose,
    url: Option<String>,
    health: &LocalHealth,
) -> LocalEndpoint {
    LocalEndpoint {
        name: name.to_string(),
        visibility,
        purpose,
        protocol: LocalEndpointProtocol::Http,
        url,
        health: health.clone(),
    }
}

/// Build an unknown-health local object-storage profile without starting a backend.
#[must_use]
pub fn object_storage_profile(config: &ObjectStorageConfig) -> LocalServiceProfile {
    config.profile(&unknown_health())
}

/// Daemon-owned object-storage service manager.
#[derive(Clone)]
pub struct ObjectStorageManager {
    inner: Arc<ObjectStorageManagerInner>,
}

struct ObjectStorageManagerInner {
    service: ObjectStorageService,
    state: Mutex<ObjectStorageManagerRecord>,
}

struct ObjectStorageManagerRecord {
    phase: ObjectStorageManagerPhase,
    running: Option<RunningObjectStorage>,
}

/// Object-storage lifecycle state surfaced through the service health profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectStorageManagerPhase {
    /// Object storage has not been started.
    NotStarted,
    /// Object storage startup is in progress.
    Starting,
    /// Object storage is running and ready.
    Running,
    /// Object storage startup or shutdown failed.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
    /// Object storage was stopped.
    Stopped,
}

impl ObjectStorageManager {
    /// Create an object-storage manager for this service.
    #[must_use]
    pub fn new(service: ObjectStorageService) -> Self {
        Self {
            inner: Arc::new(ObjectStorageManagerInner {
                service,
                state: Mutex::new(ObjectStorageManagerRecord {
                    phase: ObjectStorageManagerPhase::NotStarted,
                    running: None,
                }),
            }),
        }
    }

    /// Create an object-storage manager from config and production seams.
    #[must_use]
    pub fn from_config(config: ObjectStorageConfig) -> Self {
        Self::new(ObjectStorageService::new(config))
    }

    /// Create an object-storage manager already marked as failed.
    #[must_use]
    pub fn failed(config: ObjectStorageConfig, message: String) -> Self {
        let manager = Self::from_config(config);
        if let Ok(mut state) = manager.state() {
            state.phase = ObjectStorageManagerPhase::Failed { message };
            state.running = None;
        }
        manager
    }

    /// Start object storage and store the running service handle.
    pub async fn start(&self) -> SandboxResult<()> {
        {
            let mut state = self.state()?;
            if matches!(state.phase, ObjectStorageManagerPhase::Running) {
                return Ok(());
            }
            if matches!(state.phase, ObjectStorageManagerPhase::Starting) {
                return Err(SandboxError::provider(
                    "object storage startup already in progress",
                ));
            }
            state.phase = ObjectStorageManagerPhase::Starting;
            state.running = None;
        }

        match self.inner.service.start().await {
            Ok(running) => {
                let mut state = self.state()?;
                state.phase = ObjectStorageManagerPhase::Running;
                state.running = Some(running);
                Ok(())
            }
            Err(error) => {
                let mut state = self.state()?;
                state.phase = ObjectStorageManagerPhase::Failed {
                    message: error.to_string(),
                };
                state.running = None;
                Err(error)
            }
        }
    }

    /// Stop object storage if it is running.
    pub fn shutdown(&self) -> SandboxResult<()> {
        let running = {
            let mut state = self.state()?;
            state.phase = ObjectStorageManagerPhase::Stopped;
            state.running.take()
        };

        if let Some(running) = running {
            if let Err(error) = running.stop() {
                let mut state = self.state()?;
                state.phase = ObjectStorageManagerPhase::Failed {
                    message: error.to_string(),
                };
                return Err(error);
            }
        }

        Ok(())
    }

    /// Return the current object-storage service profile.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match &state.phase {
            ObjectStorageManagerPhase::NotStarted => {
                self.inner.service.config.profile(&unknown_health())
            }
            ObjectStorageManagerPhase::Starting => {
                self.inner.service.config.profile(&starting_health())
            }
            ObjectStorageManagerPhase::Running => state.running.as_ref().map_or_else(
                || {
                    self.inner.service.config.profile(&failed_health(
                        "object-storage running state is missing runtime handle",
                    ))
                },
                RunningObjectStorage::profile,
            ),
            ObjectStorageManagerPhase::Failed { message } => {
                self.inner.service.config.profile(&failed_health(message))
            }
            ObjectStorageManagerPhase::Stopped => {
                self.inner.service.config.profile(&stopped_health())
            }
        }
    }

    /// Return the current object-storage lifecycle state.
    #[must_use]
    pub fn phase(&self) -> ObjectStorageManagerPhase {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase
            .clone()
    }

    fn state(&self) -> SandboxResult<std::sync::MutexGuard<'_, ObjectStorageManagerRecord>> {
        self.inner
            .state
            .lock()
            .map_err(|_| SandboxError::provider("object-storage manager lock poisoned"))
    }
}

/// Local object-storage lifecycle boundary.
#[derive(Clone)]
pub struct ObjectStorageService {
    config: ObjectStorageConfig,
    embedded_runner: Arc<dyn EmbeddedObjectStorageRunner>,
    binary_resolver: Arc<dyn MinioBinaryResolver>,
    process_runner: Arc<dyn MinioProcessRunner>,
    health_probe: Arc<dyn ObjectStorageHealthProbe>,
}

impl ObjectStorageService {
    /// Create an object-storage service with production seams.
    #[must_use]
    pub fn new(config: ObjectStorageConfig) -> Self {
        Self::with_components(
            config,
            Arc::new(TokioEmbeddedObjectStorageRunner::default()),
            Arc::new(PathMinioBinaryResolver),
            Arc::new(TokioMinioProcessRunner::default()),
            Arc::new(TcpObjectStorageHealthProbe),
        )
    }

    /// Create an object-storage service from fakeable lifecycle seams.
    #[must_use]
    pub fn with_components(
        config: ObjectStorageConfig,
        embedded_runner: Arc<dyn EmbeddedObjectStorageRunner>,
        binary_resolver: Arc<dyn MinioBinaryResolver>,
        process_runner: Arc<dyn MinioProcessRunner>,
        health_probe: Arc<dyn ObjectStorageHealthProbe>,
    ) -> Self {
        Self {
            config,
            embedded_runner,
            binary_resolver,
            process_runner,
            health_probe,
        }
    }

    /// Return the configured service profile without checking readiness.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        object_storage_profile(&self.config)
    }

    /// Start object storage and wait for the configured health probe.
    pub async fn start(&self) -> SandboxResult<RunningObjectStorage> {
        match self.config.backend {
            ObjectStorageBackendKind::Embedded => self.start_embedded().await,
            ObjectStorageBackendKind::Minio => self.start_minio().await,
        }
    }

    async fn start_embedded(&self) -> SandboxResult<RunningObjectStorage> {
        std::fs::create_dir_all(&self.config.data_dir).map_err(|error| {
            SandboxError::provider(format!(
                "create object-storage data dir {}: {error}",
                self.config.data_dir.display()
            ))
        })?;
        let handle = self.embedded_runner.start(&self.config)?;
        if let Err(error) = self.health_probe.wait_until_ready(&self.config).await {
            let _ = self.embedded_runner.stop(&handle);
            return Err(error);
        }

        Ok(RunningObjectStorage {
            config: self.config.clone(),
            runtime: RunningObjectStorageRuntime::Embedded {
                handle,
                runner: Arc::clone(&self.embedded_runner),
            },
        })
    }

    async fn start_minio(&self) -> SandboxResult<RunningObjectStorage> {
        std::fs::create_dir_all(&self.config.data_dir).map_err(|error| {
            SandboxError::provider(format!(
                "create object-storage data dir {}: {error}",
                self.config.data_dir.display()
            ))
        })?;
        if let Some(log_path) = &self.config.log_path {
            if let Some(parent) = log_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    SandboxError::provider(format!(
                        "create object-storage log dir {}: {error}",
                        parent.display()
                    ))
                })?;
            }
        }

        let binary = self.binary_resolver.resolve(&self.config)?;
        let handle = self.process_runner.start(&self.config, binary.as_path())?;
        if let Err(error) = self.health_probe.wait_until_ready(&self.config).await {
            let _ = self.process_runner.stop(&handle);
            return Err(error);
        }

        Ok(RunningObjectStorage {
            config: self.config.clone(),
            runtime: RunningObjectStorageRuntime::Minio {
                handle,
                runner: Arc::clone(&self.process_runner),
            },
        })
    }
}

/// Running object-storage service handle.
#[derive(Clone)]
pub struct RunningObjectStorage {
    config: ObjectStorageConfig,
    runtime: RunningObjectStorageRuntime,
}

#[derive(Clone)]
enum RunningObjectStorageRuntime {
    Embedded {
        handle: EmbeddedObjectStorageHandle,
        runner: Arc<dyn EmbeddedObjectStorageRunner>,
    },
    Minio {
        handle: MinioProcessHandle,
        runner: Arc<dyn MinioProcessRunner>,
    },
}

impl RunningObjectStorage {
    /// Return the ready service profile.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        self.config.profile(&ready_health())
    }

    /// Stop the running object-storage backend.
    pub fn stop(&self) -> SandboxResult<()> {
        match &self.runtime {
            RunningObjectStorageRuntime::Embedded { handle, runner } => runner.stop(handle),
            RunningObjectStorageRuntime::Minio { handle, runner } => runner.stop(handle),
        }
    }
}

/// Fakeable embedded object-storage runner.
pub trait EmbeddedObjectStorageRunner: Send + Sync {
    /// Start embedded object storage and return a handle.
    fn start(&self, config: &ObjectStorageConfig) -> SandboxResult<EmbeddedObjectStorageHandle>;

    /// Stop embedded object storage.
    fn stop(&self, handle: &EmbeddedObjectStorageHandle) -> SandboxResult<()>;
}

/// Handle returned by an embedded object-storage runner.
#[derive(Clone)]
pub struct EmbeddedObjectStorageHandle {
    /// Runner-local handle identifier.
    pub id: String,
}

struct EmbeddedObjectStorageServer {
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

/// Fakeable `MinIO` binary resolver.
pub trait MinioBinaryResolver: Send + Sync {
    /// Resolve the `MinIO` binary for this configuration.
    fn resolve(&self, config: &ObjectStorageConfig) -> SandboxResult<PathBuf>;
}

/// Fakeable `MinIO` process runner.
pub trait MinioProcessRunner: Send + Sync {
    /// Start a `MinIO` server process.
    fn start(
        &self,
        config: &ObjectStorageConfig,
        binary: &std::path::Path,
    ) -> SandboxResult<MinioProcessHandle>;

    /// Stop a `MinIO` server process.
    fn stop(&self, handle: &MinioProcessHandle) -> SandboxResult<()>;
}

/// Fakeable object-storage health probe.
pub trait ObjectStorageHealthProbe: Send + Sync {
    /// Wait until object storage is reachable.
    fn wait_until_ready<'a>(
        &'a self,
        config: &'a ObjectStorageConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SandboxResult<()>> + Send + 'a>>;
}

/// Handle returned by a `MinIO` process runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinioProcessHandle {
    /// Runner-defined process identifier.
    pub id: String,
}

/// Resolve the configured `MinIO` binary or `minio` from `PATH`.
#[derive(Default)]
pub struct PathMinioBinaryResolver;

impl MinioBinaryResolver for PathMinioBinaryResolver {
    fn resolve(&self, config: &ObjectStorageConfig) -> SandboxResult<PathBuf> {
        config
            .minio_binary
            .clone()
            .or_else(|| resolve_path_binary("minio"))
            .ok_or_else(|| SandboxError::provider("minio binary not found on PATH"))
    }
}

/// Run embedded S3-compatible object storage with `s3s-fs`.
#[derive(Default)]
pub struct TokioEmbeddedObjectStorageRunner {
    servers: Mutex<BTreeMap<String, EmbeddedObjectStorageServer>>,
}

impl EmbeddedObjectStorageRunner for TokioEmbeddedObjectStorageRunner {
    fn start(&self, config: &ObjectStorageConfig) -> SandboxResult<EmbeddedObjectStorageHandle> {
        let filesystem = FileSystem::new(&config.data_dir).map_err(|error| {
            SandboxError::provider(format!(
                "start embedded object storage at {}: {error:?}",
                config.data_dir.display()
            ))
        })?;
        let service = {
            let mut builder = S3ServiceBuilder::new(filesystem);
            builder.set_auth(SimpleAuth::from_single(
                config.access_key_id.clone(),
                config.secret_access_key.clone(),
            ));
            builder.build()
        };

        let bind_host = config.api_host.parse::<IpAddr>().map_err(|error| {
            SandboxError::invalid_request(format!(
                "object-storage API host must be an IP address: {error}"
            ))
        })?;
        let bind_addr = (bind_host, config.api_port);
        let std_listener = std::net::TcpListener::bind(bind_addr).map_err(|error| {
            SandboxError::provider(format!(
                "bind embedded object storage {}:{}: {error}",
                config.api_host, config.api_port
            ))
        })?;
        std_listener.set_nonblocking(true).map_err(|error| {
            SandboxError::provider(format!(
                "configure embedded object storage listener: {error}"
            ))
        })?;
        let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|error| {
            SandboxError::provider(format!("create embedded object storage listener: {error}"))
        })?;

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let id = format!("embedded-s3-{}", config.api_port);
        let task = tokio::spawn(async move {
            let http_server = ConnBuilder::new(TokioExecutor::new());
            let graceful = GracefulShutdown::new();
            loop {
                let (socket, _) = tokio::select! {
                    result = listener.accept() => match result {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(error = %error, "embedded object storage accept failed");
                            continue;
                        }
                    },
                    _ = &mut shutdown_rx => break,
                };

                let connection =
                    http_server.serve_connection(TokioIo::new(socket), service.clone());
                let connection = graceful.watch(connection.into_owned());
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::warn!(error = %error, "embedded object storage connection failed");
                    }
                });
            }

            tokio::select! {
                () = graceful.shutdown() => {}
                () = sleep(Duration::from_secs(10)) => {
                    tracing::warn!("timed out waiting for embedded object storage connections to drain");
                }
            }
        });

        self.servers
            .lock()
            .map_err(|_| SandboxError::provider("embedded object-storage registry lock poisoned"))?
            .insert(
                id.clone(),
                EmbeddedObjectStorageServer {
                    shutdown: Some(shutdown_tx),
                    task,
                },
            );

        Ok(EmbeddedObjectStorageHandle { id })
    }

    fn stop(&self, handle: &EmbeddedObjectStorageHandle) -> SandboxResult<()> {
        let mut server = self
            .servers
            .lock()
            .map_err(|_| SandboxError::provider("embedded object-storage registry lock poisoned"))?
            .remove(&handle.id)
            .ok_or_else(|| SandboxError::provider("embedded object-storage handle is unknown"))?;
        if let Some(shutdown) = server.shutdown.take() {
            let _ = shutdown.send(());
        }
        tokio::spawn(async move {
            if let Err(error) = server.task.await {
                if !error.is_cancelled() {
                    tracing::warn!(error = %error, "embedded object storage task failed during shutdown");
                }
            }
        });
        Ok(())
    }
}

/// Spawn `MinIO` with `tokio::process::Command`.
#[derive(Default)]
pub struct TokioMinioProcessRunner {
    children: Mutex<BTreeMap<String, tokio::process::Child>>,
}

impl MinioProcessRunner for TokioMinioProcessRunner {
    fn start(
        &self,
        config: &ObjectStorageConfig,
        binary: &std::path::Path,
    ) -> SandboxResult<MinioProcessHandle> {
        let stdout = log_stdio(config)?;
        let stderr = log_stdio(config)?;
        let child = Command::new(binary)
            .arg("server")
            .arg(&config.data_dir)
            .arg("--address")
            .arg(format!("{}:{}", config.api_host, config.api_port))
            .arg("--console-address")
            .arg(format!("{}:{}", config.console_host, config.console_port))
            .env("MINIO_ROOT_USER", &config.access_key_id)
            .env("MINIO_ROOT_PASSWORD", &config.secret_access_key)
            .env("MINIO_REGION_NAME", &config.region)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|error| SandboxError::provider(format!("start minio: {error}")))?;
        let id = child
            .id()
            .map_or_else(|| "minio".to_string(), |id| id.to_string());

        self.children
            .lock()
            .map_err(|_| SandboxError::provider("minio process registry lock poisoned"))?
            .insert(id.clone(), child);

        Ok(MinioProcessHandle { id })
    }

    fn stop(&self, handle: &MinioProcessHandle) -> SandboxResult<()> {
        let mut child = self
            .children
            .lock()
            .map_err(|_| SandboxError::provider("minio process registry lock poisoned"))?
            .remove(&handle.id)
            .ok_or_else(|| SandboxError::provider("minio process handle is unknown"))?;
        child
            .start_kill()
            .map_err(|error| SandboxError::provider(format!("stop minio: {error}")))?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}

/// TCP readiness probe for the `MinIO` API listener.
#[derive(Default)]
pub struct TcpObjectStorageHealthProbe;

impl ObjectStorageHealthProbe for TcpObjectStorageHealthProbe {
    fn wait_until_ready<'a>(
        &'a self,
        config: &'a ObjectStorageConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SandboxResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            loop {
                if TcpStream::connect((config.probe_host(), config.api_port))
                    .await
                    .is_ok()
                {
                    return Ok(());
                }

                if started.elapsed() >= config.readiness_timeout {
                    return Err(SandboxError::provider(format!(
                        "minio did not become ready on {}:{}",
                        config.probe_host(),
                        config.api_port
                    )));
                }

                sleep(Duration::from_millis(100)).await;
            }
        })
    }
}

fn resolve_path_binary(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn log_stdio(config: &ObjectStorageConfig) -> SandboxResult<Stdio> {
    config.log_path.as_ref().map_or_else(
        || Ok(Stdio::null()),
        |path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map(Stdio::from)
                .map_err(|error| {
                    SandboxError::provider(format!(
                        "open object-storage log file {}: {error}",
                        path.display()
                    ))
                })
        },
    )
}

fn read_env_path(read: &mut impl FnMut(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    read(name)
        .filter(|value| !value.as_os_str().is_empty())
        .map(PathBuf::from)
}

fn read_env_string(
    read: &mut impl FnMut(&str) -> Option<OsString>,
    name: &str,
) -> SandboxResult<Option<String>> {
    read(name)
        .filter(|value| !value.as_os_str().is_empty())
        .map(|value| {
            value.into_string().map_err(|_| {
                SandboxError::invalid_request(format!("{name} must contain valid unicode"))
            })
        })
        .transpose()
}

fn read_env_u16(
    read: &mut impl FnMut(&str) -> Option<OsString>,
    name: &str,
) -> SandboxResult<Option<u16>> {
    read_env_string(read, name)?.map_or(Ok(None), |value| {
        value.parse::<u16>().map(Some).map_err(|error| {
            SandboxError::invalid_request(format!("{name} must be a TCP port: {error}"))
        })
    })
}

fn host_endpoint_host_for_api_host(api_host: &str) -> String {
    if api_host == "0.0.0.0" || api_host == "::" {
        "127.0.0.1".to_string()
    } else {
        api_host.to_string()
    }
}

fn ready_health() -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Ready,
        message: None,
        checked_at_unix_ms: None,
    }
}

fn unknown_health() -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Unknown,
        message: Some("not started".to_string()),
        checked_at_unix_ms: None,
    }
}

fn starting_health() -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Starting,
        message: Some("starting".to_string()),
        checked_at_unix_ms: None,
    }
}

fn failed_health(message: &str) -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Failed,
        message: Some(message.to_string()),
        checked_at_unix_ms: None,
    }
}

fn stopped_health() -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Stopped,
        message: Some("stopped".to_string()),
        checked_at_unix_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, future::Future, path::Path, pin::Pin, sync::Mutex};

    #[test]
    fn object_storage_config_reads_product_neutral_environment() {
        let env = BTreeMap::from([
            (ENV_OBJECT_STORAGE_BACKEND, "minio"),
            (ENV_MINIO_BINARY, "/opt/minio"),
            (ENV_OBJECT_STORAGE_DATA_DIR, "/var/local-sandbox/minio-data"),
            (ENV_OBJECT_STORAGE_API_HOST, "0.0.0.0"),
            (ENV_OBJECT_STORAGE_API_PORT, "19000"),
            (ENV_OBJECT_STORAGE_CONSOLE_PORT, "19001"),
            (ENV_OBJECT_STORAGE_REGION, "local-test-1"),
            (ENV_OBJECT_STORAGE_ACCESS_KEY_ID, "test-access-key"),
            (ENV_OBJECT_STORAGE_SECRET_ACCESS_KEY, "test-secret-key"),
            (ENV_OBJECT_STORAGE_GUEST_HOST, "host.internal"),
            (
                ENV_OBJECT_STORAGE_LOG_PATH,
                "/var/log/local-sandbox/minio.log",
            ),
        ]);

        let config = ObjectStorageConfig::from_env_reader(|name| {
            env.get(name)
                .map(|value| OsString::from((*value).to_string()))
        })
        .unwrap();

        assert_eq!(config.backend, ObjectStorageBackendKind::Minio);
        assert_eq!(config.minio_binary, Some(PathBuf::from("/opt/minio")));
        assert_eq!(
            config.data_dir,
            PathBuf::from("/var/local-sandbox/minio-data")
        );
        assert_eq!(config.api_host, "0.0.0.0");
        assert_eq!(config.console_host, "0.0.0.0");
        assert_eq!(config.api_port, 19_000);
        assert_eq!(config.console_port, 19_001);
        assert_eq!(config.host_endpoint(), "http://127.0.0.1:19000");
        assert_eq!(config.guest_endpoint(), "http://host.internal:19000");
        assert_eq!(config.region, "local-test-1");
        assert_eq!(config.access_key_id, "test-access-key");
        assert_eq!(config.secret_access_key, "test-secret-key");
        assert_eq!(
            config.log_path,
            Some(PathBuf::from("/var/log/local-sandbox/minio.log"))
        );
    }

    #[test]
    fn object_storage_config_defaults_to_embedded_backend() {
        let config = ObjectStorageConfig::from_env_reader(|_| None).unwrap();

        assert_eq!(config.backend, ObjectStorageBackendKind::Embedded);
        assert_eq!(config.minio_binary, None);
    }

    #[test]
    fn object_storage_config_rejects_invalid_ports() {
        let result = ObjectStorageConfig::from_env_reader(|name| {
            (name == ENV_OBJECT_STORAGE_API_PORT).then(|| OsString::from("not-a-port"))
        });

        assert!(matches!(
            result,
            Err(SandboxError::InvalidRequest { message }) if message.contains(ENV_OBJECT_STORAGE_API_PORT)
        ));
    }

    #[test]
    fn object_storage_profile_contains_minio_facts_and_bounded_capabilities() {
        let config = ObjectStorageConfig {
            data_dir: PathBuf::from("/tmp/local-sandbox/minio-data"),
            host_endpoint_host: "127.0.0.1".to_string(),
            guest_endpoint_host: "host.internal".to_string(),
            log_path: Some(PathBuf::from("/tmp/local-sandbox/minio.log")),
            ..ObjectStorageConfig::default()
        };

        let profile = object_storage_profile(&config);

        assert_eq!(profile.health.status, LocalHealthStatus::Unknown);
        assert_eq!(profile.endpoints.len(), 2);
        assert!(profile.endpoints.iter().any(|endpoint| {
            endpoint.name == "host_api"
                && endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.url.as_deref() == Some("http://127.0.0.1:9000")
        }));
        assert!(profile.endpoints.iter().any(|endpoint| {
            endpoint.name == "guest_api"
                && endpoint.visibility == LocalEndpointVisibility::Guest
                && endpoint.url.as_deref() == Some("http://host.internal:9000")
        }));
        assert_eq!(
            profile
                .credentials
                .as_ref()
                .unwrap()
                .access_key_id
                .as_deref(),
            Some(DEFAULT_ACCESS_KEY_ID)
        );
        assert_eq!(
            profile.logs[0].path.as_deref(),
            Some("/tmp/local-sandbox/minio.log")
        );

        let object_storage = profile.object_storage.as_ref().unwrap();
        assert_eq!(object_storage.region, DEFAULT_REGION);
        assert!(object_storage.force_path_style);
        assert!(object_storage.capabilities.create_bucket);
        assert!(object_storage.capabilities.put_object);
        assert!(object_storage.capabilities.get_object);
        assert!(object_storage.capabilities.head_object);
        assert!(object_storage.capabilities.delete_object);
        assert!(object_storage.capabilities.delete_objects);
        assert!(object_storage.capabilities.list_objects_v2);
        assert!(object_storage.capabilities.copy_object);
        assert!(object_storage.capabilities.presigned_get);
        assert!(object_storage.capabilities.presigned_put);
        assert!(!object_storage.capabilities.cors);
        assert!(!object_storage.capabilities.browser_upload);
        assert!(!object_storage.capabilities.versioning);
        assert!(!object_storage.capabilities.list_object_versions);
    }

    #[tokio::test]
    async fn object_storage_start_uses_fakeable_seams_and_returns_ready_profile() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir().join(format!("local-sandbox-test-{}", std::process::id())),
            backend: ObjectStorageBackendKind::Minio,
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let resolver = Arc::new(FakeResolver);
        let runner = Arc::new(FakeRunner::default());
        let probe = Arc::new(FakeProbe);
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner::default()),
            resolver,
            runner.clone(),
            probe,
        );

        let running = service.start().await.unwrap();

        assert_eq!(
            runner.started.lock().unwrap().as_slice(),
            std::slice::from_ref(&config.data_dir)
        );
        assert_eq!(running.profile().health.status, LocalHealthStatus::Ready);
        running.stop().unwrap();
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-minio".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn object_storage_start_uses_embedded_backend_by_default() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-embedded-default-test-{}",
                std::process::id()
            )),
            api_port: 19_100,
            ..ObjectStorageConfig::default()
        };
        let embedded = Arc::new(FakeEmbeddedRunner::default());
        let runner = Arc::new(FakeRunner::default());
        let service = ObjectStorageService::with_components(
            config.clone(),
            embedded.clone(),
            Arc::new(FakeResolver),
            runner.clone(),
            Arc::new(FakeProbe),
        );

        let running = service.start().await.unwrap();

        assert_eq!(
            embedded.started.lock().unwrap().as_slice(),
            std::slice::from_ref(&config.data_dir)
        );
        assert!(runner.started.lock().unwrap().is_empty());
        assert_eq!(running.profile().health.status, LocalHealthStatus::Ready);
        running.stop().unwrap();
        assert_eq!(
            embedded.stopped.lock().unwrap().as_slice(),
            &["fake-embedded-s3".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn object_storage_start_stops_process_when_health_fails() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir()
                .join(format!("local-sandbox-test-fail-{}", std::process::id())),
            backend: ObjectStorageBackendKind::Minio,
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner::default()),
            Arc::new(FakeResolver),
            runner.clone(),
            Arc::new(FailingProbe),
        );

        let result = service.start().await;

        assert!(
            matches!(result, Err(SandboxError::Provider { message }) if message == "health failed")
        );
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-minio".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn object_storage_manager_stores_running_service_and_stops_on_shutdown() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir()
                .join(format!("local-sandbox-manager-test-{}", std::process::id())),
            backend: ObjectStorageBackendKind::Minio,
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner::default()),
            Arc::new(FakeResolver),
            runner.clone(),
            Arc::new(FakeProbe),
        );
        let manager = ObjectStorageManager::new(service);

        assert_eq!(manager.profile().health.status, LocalHealthStatus::Unknown);
        manager.start().await.unwrap();
        assert_eq!(manager.profile().health.status, LocalHealthStatus::Ready);
        manager.shutdown().unwrap();

        assert_eq!(manager.profile().health.status, LocalHealthStatus::Stopped);
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-minio".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn object_storage_manager_keeps_failed_profile_after_start_failure() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-manager-fail-test-{}",
                std::process::id()
            )),
            backend: ObjectStorageBackendKind::Minio,
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner::default()),
            Arc::new(FakeResolver),
            Arc::new(FakeRunner::default()),
            Arc::new(FailingProbe),
        );
        let manager = ObjectStorageManager::new(service);

        let result = manager.start().await;

        assert!(result.is_err());
        let profile = manager.profile();
        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert!(profile.health.message.unwrap().contains("health failed"));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn object_storage_manager_rejects_concurrent_startup() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-manager-slow-test-{}",
                std::process::id()
            )),
            backend: ObjectStorageBackendKind::Minio,
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner::default()),
            Arc::new(FakeResolver),
            Arc::new(FakeRunner::default()),
            Arc::new(SlowProbe),
        );
        let manager = ObjectStorageManager::new(service);
        let first_start_manager = manager.clone();
        let first_start = tokio::spawn(async move { first_start_manager.start().await });

        while !matches!(manager.phase(), ObjectStorageManagerPhase::Starting) {
            tokio::task::yield_now().await;
        }

        let second_start = manager.start().await;

        assert!(
            matches!(second_start, Err(SandboxError::Provider { message }) if message == "object storage startup already in progress")
        );
        first_start.await.unwrap().unwrap();
        manager.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn object_storage_manager_reports_missing_running_handle_as_failed() {
        let manager = ObjectStorageManager::from_config(ObjectStorageConfig::default());
        manager.set_phase_for_test(ObjectStorageManagerPhase::Running);

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert_eq!(
            profile.health.message.as_deref(),
            Some("object-storage running state is missing runtime handle")
        );
    }

    #[test]
    fn object_storage_manager_failed_constructor_profiles_error() {
        let manager = ObjectStorageManager::failed(
            ObjectStorageConfig::default(),
            "invalid object-storage config".to_string(),
        );

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert_eq!(
            profile.health.message.as_deref(),
            Some("invalid object-storage config")
        );
    }

    #[test]
    fn object_storage_manager_profiles_starting_state() {
        let manager = ObjectStorageManager::from_config(ObjectStorageConfig::default());
        manager.set_phase_for_test(ObjectStorageManagerPhase::Starting);

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Starting);
        assert_eq!(profile.health.message.as_deref(), Some("starting"));
    }

    impl ObjectStorageManager {
        fn set_phase_for_test(&self, phase: ObjectStorageManagerPhase) {
            self.inner.state.lock().unwrap().phase = phase;
        }
    }

    #[derive(Default)]
    struct FakeEmbeddedRunner {
        started: Mutex<Vec<PathBuf>>,
        stopped: Mutex<Vec<String>>,
    }

    impl EmbeddedObjectStorageRunner for FakeEmbeddedRunner {
        fn start(
            &self,
            config: &ObjectStorageConfig,
        ) -> SandboxResult<EmbeddedObjectStorageHandle> {
            self.started.lock().unwrap().push(config.data_dir.clone());
            Ok(EmbeddedObjectStorageHandle {
                id: "fake-embedded-s3".to_string(),
            })
        }

        fn stop(&self, handle: &EmbeddedObjectStorageHandle) -> SandboxResult<()> {
            self.stopped.lock().unwrap().push(handle.id.clone());
            Ok(())
        }
    }

    struct FakeResolver;

    impl MinioBinaryResolver for FakeResolver {
        fn resolve(&self, config: &ObjectStorageConfig) -> SandboxResult<PathBuf> {
            Ok(config.minio_binary.clone().unwrap())
        }
    }

    #[derive(Default)]
    struct FakeRunner {
        started: Mutex<Vec<PathBuf>>,
        stopped: Mutex<Vec<String>>,
    }

    impl MinioProcessRunner for FakeRunner {
        fn start(
            &self,
            config: &ObjectStorageConfig,
            binary: &Path,
        ) -> SandboxResult<MinioProcessHandle> {
            assert_eq!(binary, Path::new("/fake/minio"));
            self.started.lock().unwrap().push(config.data_dir.clone());
            Ok(MinioProcessHandle {
                id: "fake-minio".to_string(),
            })
        }

        fn stop(&self, handle: &MinioProcessHandle) -> SandboxResult<()> {
            self.stopped.lock().unwrap().push(handle.id.clone());
            Ok(())
        }
    }

    struct FakeProbe;

    impl ObjectStorageHealthProbe for FakeProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a ObjectStorageConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FailingProbe;

    impl ObjectStorageHealthProbe for FailingProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a ObjectStorageConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async { Err(SandboxError::provider("health failed")) })
        }
    }

    struct SlowProbe;

    impl ObjectStorageHealthProbe for SlowProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a ObjectStorageConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async {
                tokio::task::yield_now().await;
                Ok(())
            })
        }
    }
}
