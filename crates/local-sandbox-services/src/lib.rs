//! Local service orchestration boundary.

use local_sandbox_core::{
    LocalCredentials, LocalEndpoint, LocalEndpointProtocol, LocalEndpointPurpose,
    LocalEndpointVisibility, LocalHealth, LocalHealthStatus, LocalLogProfile,
    LocalObjectStorageCapabilities, LocalObjectStorageProfile, LocalServiceProfile, SandboxError,
    SandboxResult,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpStream, process::Command, time::sleep};

const DEFAULT_MINIO_API_PORT: u16 = 9_000;
const DEFAULT_MINIO_CONSOLE_PORT: u16 = 9_001;
const DEFAULT_ACCESS_KEY_ID: &str = "local-sandbox";
const DEFAULT_SECRET_ACCESS_KEY: &str = "local-sandbox-secret";
const DEFAULT_REGION: &str = "us-east-1";

/// Local service kind managed by the substrate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalServiceKind {
    /// S3-compatible object storage.
    S3,
    /// `PostgreSQL` database.
    Postgres,
}

/// Configuration for local S3-compatible object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectStorageConfig {
    /// Directory used for object-storage data.
    pub data_dir: PathBuf,
    /// Path to the `MinIO` server binary. If absent, `minio` is resolved from `PATH`.
    pub minio_binary: Option<PathBuf>,
    /// Host interface used by the `MinIO` API listener.
    pub api_host: String,
    /// Host port used by the `MinIO` API listener.
    pub api_port: u16,
    /// Host interface used by the `MinIO` console listener.
    pub console_host: String,
    /// Host port used by the `MinIO` console listener.
    pub console_port: u16,
    /// Host name used by clients in the host process namespace.
    pub host_endpoint_host: String,
    /// Host name used by clients inside sandbox guests.
    pub guest_endpoint_host: String,
    /// Region used by S3-compatible clients.
    pub region: String,
    /// Access key identifier for `MinIO` root credentials.
    pub access_key_id: String,
    /// Secret access key for `MinIO` root credentials.
    pub secret_access_key: String,
    /// Optional file path where `MinIO` logs are written by the process runner.
    pub log_path: Option<PathBuf>,
    /// Startup readiness deadline.
    pub readiness_timeout: Duration,
}

impl Default for ObjectStorageConfig {
    fn default() -> Self {
        let api_host = "127.0.0.1".to_string();
        let api_port = DEFAULT_MINIO_API_PORT;
        let console_host = "127.0.0.1".to_string();
        let console_port = DEFAULT_MINIO_CONSOLE_PORT;

        Self {
            data_dir: env::temp_dir().join("local-sandbox").join("minio-data"),
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
            log_path: Some(env::temp_dir().join("local-sandbox").join("minio.log")),
            readiness_timeout: Duration::from_secs(30),
        }
    }
}

impl ObjectStorageConfig {
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

/// Build an unknown-health local object-storage profile without starting `MinIO`.
#[must_use]
pub fn object_storage_profile(config: &ObjectStorageConfig) -> LocalServiceProfile {
    config.profile(&unknown_health())
}

/// Local object-storage lifecycle boundary.
#[derive(Clone)]
pub struct ObjectStorageService {
    config: ObjectStorageConfig,
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
            Arc::new(PathMinioBinaryResolver),
            Arc::new(TokioMinioProcessRunner::default()),
            Arc::new(TcpObjectStorageHealthProbe),
        )
    }

    /// Create an object-storage service from fakeable lifecycle seams.
    #[must_use]
    pub fn with_components(
        config: ObjectStorageConfig,
        binary_resolver: Arc<dyn MinioBinaryResolver>,
        process_runner: Arc<dyn MinioProcessRunner>,
        health_probe: Arc<dyn ObjectStorageHealthProbe>,
    ) -> Self {
        Self {
            config,
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

    /// Start `MinIO` and wait for the configured health probe.
    pub async fn start(&self) -> SandboxResult<RunningObjectStorage> {
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
            process: handle,
            process_runner: Arc::clone(&self.process_runner),
        })
    }
}

/// Running object-storage service handle.
pub struct RunningObjectStorage {
    config: ObjectStorageConfig,
    process: MinioProcessHandle,
    process_runner: Arc<dyn MinioProcessRunner>,
}

impl RunningObjectStorage {
    /// Return the ready service profile.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        self.config.profile(&ready_health())
    }

    /// Stop the `MinIO` process.
    pub fn stop(&self) -> SandboxResult<()> {
        self.process_runner.stop(&self.process)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, path::Path, pin::Pin, sync::Mutex};

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
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let resolver = Arc::new(FakeResolver);
        let runner = Arc::new(FakeRunner::default());
        let probe = Arc::new(FakeProbe);
        let service =
            ObjectStorageService::with_components(config.clone(), resolver, runner.clone(), probe);

        let running = service.start().await.unwrap();

        assert_eq!(
            runner.started.lock().unwrap().as_slice(),
            &[config.data_dir.clone()]
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
    async fn object_storage_start_stops_process_when_health_fails() {
        let config = ObjectStorageConfig {
            data_dir: env::temp_dir()
                .join(format!("local-sandbox-test-fail-{}", std::process::id())),
            minio_binary: Some(PathBuf::from("/fake/minio")),
            readiness_timeout: Duration::from_millis(1),
            ..ObjectStorageConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service = ObjectStorageService::with_components(
            config.clone(),
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
}
