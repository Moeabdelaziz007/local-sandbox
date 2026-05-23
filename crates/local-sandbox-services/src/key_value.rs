//! Redis-compatible key-value service orchestration.

use local_sandbox_core::{
    LocalCredentials, LocalEndpoint, LocalEndpointProtocol, LocalEndpointPurpose,
    LocalEndpointVisibility, LocalHealth, LocalKeyValueCapabilities, LocalKeyValueProfile,
    LocalServiceProfile, SandboxError, SandboxResult,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tinyredis::{config::Config as TinyredisConfig, server, stats::ServerStats, store::Store};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{broadcast, Mutex as TokioMutex},
    task::JoinHandle,
    time::sleep,
};

use super::{
    failed_health, host_endpoint_host_for_api_host, read_env_path, read_env_string, read_env_u16,
    ready_health, starting_health, stopped_health, unknown_health,
};

const DEFAULT_KEY_VALUE_PORT: u16 = 16_379;

/// Environment variable for key-value data directory.
pub const ENV_KEY_VALUE_DATA_DIR: &str = "LOCAL_SANDBOX_KEY_VALUE_DATA_DIR";
/// Environment variable for key-value bind host.
pub const ENV_KEY_VALUE_HOST: &str = "LOCAL_SANDBOX_KEY_VALUE_HOST";
/// Environment variable for key-value port.
pub const ENV_KEY_VALUE_PORT: &str = "LOCAL_SANDBOX_KEY_VALUE_PORT";
/// Environment variable for the guest-visible key-value host.
pub const ENV_KEY_VALUE_GUEST_HOST: &str = "LOCAL_SANDBOX_KEY_VALUE_GUEST_HOST";
/// Environment variable for optional Redis-compatible password auth.
pub const ENV_KEY_VALUE_PASSWORD: &str = "LOCAL_SANDBOX_KEY_VALUE_PASSWORD";

/// Key-value backend implementation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyValueBackendKind {
    /// Embedded patched `tinyredis` backend.
    #[default]
    Tinyredis,
}

/// Configuration for local Redis-compatible key-value storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyValueConfig {
    /// Backend implementation to use for key-value storage.
    pub backend: KeyValueBackendKind,
    /// Directory reserved for key-value data and future persistence artifacts.
    pub data_dir: PathBuf,
    /// Host interface used by the Redis-compatible listener.
    pub host: String,
    /// Host port used by the Redis-compatible listener.
    pub port: u16,
    /// Host name used by clients in the host process namespace.
    pub host_endpoint_host: String,
    /// Host name used by clients inside sandbox guests.
    pub guest_endpoint_host: String,
    /// Optional password required through Redis `AUTH`.
    pub password: Option<String>,
    /// Startup readiness deadline.
    pub readiness_timeout: Duration,
}

impl Default for KeyValueConfig {
    fn default() -> Self {
        Self {
            backend: KeyValueBackendKind::Tinyredis,
            data_dir: env::temp_dir().join("local-sandbox").join("key-value-data"),
            host: "127.0.0.1".to_string(),
            port: DEFAULT_KEY_VALUE_PORT,
            host_endpoint_host: "127.0.0.1".to_string(),
            guest_endpoint_host: "host.internal".to_string(),
            password: None,
            readiness_timeout: Duration::from_secs(30),
        }
    }
}

impl KeyValueConfig {
    /// Build key-value config from product-neutral local-sandbox environment variables.
    pub fn from_env() -> SandboxResult<Self> {
        Self::from_env_reader(|name| env::var_os(name))
    }

    fn from_env_reader(mut read: impl FnMut(&str) -> Option<OsString>) -> SandboxResult<Self> {
        let mut config = Self::default();

        if let Some(value) = read_env_path(&mut read, ENV_KEY_VALUE_DATA_DIR) {
            config.data_dir = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_KEY_VALUE_HOST)? {
            validate_bind_host(ENV_KEY_VALUE_HOST, &value)?;
            config.host_endpoint_host = host_endpoint_host_for_api_host(&value);
            config.host = value;
        }
        if let Some(value) = read_env_u16(&mut read, ENV_KEY_VALUE_PORT)? {
            config.port = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_KEY_VALUE_GUEST_HOST)? {
            config.guest_endpoint_host = value;
        }
        if let Some(value) = read_env_string(&mut read, ENV_KEY_VALUE_PASSWORD)? {
            config.password = Some(value);
        }

        Ok(config)
    }

    /// Endpoint reachable from the host process namespace.
    #[must_use]
    pub fn host_endpoint(&self) -> String {
        format!("redis://{}:{}", self.host_endpoint_host, self.port)
    }

    /// Endpoint reachable from sandbox guests.
    #[must_use]
    pub fn guest_endpoint(&self) -> String {
        format!("redis://{}:{}", self.guest_endpoint_host, self.port)
    }

    /// Hostname to use for local readiness probes.
    #[must_use]
    pub fn probe_host(&self) -> &str {
        if self.host == "0.0.0.0" || self.host == "::" {
            &self.host_endpoint_host
        } else {
            &self.host
        }
    }

    /// Return the product-neutral Redis-compatible operation support matrix.
    #[must_use]
    pub const fn capabilities() -> LocalKeyValueCapabilities {
        LocalKeyValueCapabilities {
            ping: true,
            strings: true,
            expiry: true,
            hashes: true,
            lists: true,
            sets: true,
            sorted_sets: true,
            scan: true,
            pipeline: true,
            transactions: true,
            targeted_lock_scripts: true,
            lua_scripts: false,
        }
    }

    /// Build the service profile for this key-value configuration and health.
    #[must_use]
    pub fn profile(&self, health: &LocalHealth) -> LocalServiceProfile {
        LocalServiceProfile {
            health: health.clone(),
            endpoints: vec![
                endpoint(
                    "host_redis",
                    LocalEndpointVisibility::Host,
                    Some(self.host_endpoint()),
                    health,
                ),
                endpoint(
                    "guest_redis",
                    LocalEndpointVisibility::Guest,
                    Some(self.guest_endpoint()),
                    health,
                ),
            ],
            credentials: Some(LocalCredentials {
                access_key_id: None,
                secret_access_key: None,
                username: None,
                password: self.password.clone(),
                token: None,
            }),
            logs: Vec::new(),
            object_storage: None,
            key_value: Some(LocalKeyValueProfile {
                database: 0,
                implementation: "tinyredis".to_string(),
                capabilities: Self::capabilities(),
            }),
        }
    }
}

fn validate_bind_host(name: &str, value: &str) -> SandboxResult<()> {
    value.parse::<IpAddr>().map(|_| ()).map_err(|error| {
        SandboxError::invalid_request(format!("{name} must be an IP address: {error}"))
    })
}

fn endpoint(
    name: &str,
    visibility: LocalEndpointVisibility,
    url: Option<String>,
    health: &LocalHealth,
) -> LocalEndpoint {
    LocalEndpoint {
        name: name.to_string(),
        visibility,
        purpose: LocalEndpointPurpose::KeyValue,
        protocol: LocalEndpointProtocol::Redis,
        url,
        health: health.clone(),
    }
}

/// Build an unknown-health local key-value profile without starting a backend.
#[must_use]
pub fn key_value_profile(config: &KeyValueConfig) -> LocalServiceProfile {
    config.profile(&unknown_health())
}

/// Daemon-owned key-value service manager.
#[derive(Clone)]
pub struct KeyValueManager {
    inner: Arc<KeyValueManagerInner>,
}

struct KeyValueManagerInner {
    service: KeyValueService,
    state: Mutex<KeyValueManagerRecord>,
}

struct KeyValueManagerRecord {
    phase: KeyValueManagerPhase,
    running: Option<RunningKeyValue>,
}

/// Key-value lifecycle state surfaced through the service health profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyValueManagerPhase {
    /// Key-value storage has not been started.
    NotStarted,
    /// Key-value startup is in progress.
    Starting,
    /// Key-value storage is running and ready.
    Running,
    /// Key-value startup or shutdown failed.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
    /// Key-value storage was stopped.
    Stopped,
}

impl KeyValueManager {
    /// Create a key-value manager for this service.
    #[must_use]
    pub fn new(service: KeyValueService) -> Self {
        Self {
            inner: Arc::new(KeyValueManagerInner {
                service,
                state: Mutex::new(KeyValueManagerRecord {
                    phase: KeyValueManagerPhase::NotStarted,
                    running: None,
                }),
            }),
        }
    }

    /// Create a key-value manager from config and production seams.
    #[must_use]
    pub fn from_config(config: KeyValueConfig) -> Self {
        Self::new(KeyValueService::new(config))
    }

    /// Create a key-value manager already marked as failed.
    #[must_use]
    pub fn failed(config: KeyValueConfig, message: String) -> Self {
        let manager = Self::from_config(config);
        if let Ok(mut state) = manager.state() {
            state.phase = KeyValueManagerPhase::Failed { message };
            state.running = None;
        }
        manager
    }

    /// Start key-value storage and store the running service handle.
    pub async fn start(&self) -> SandboxResult<()> {
        {
            let mut state = self.state()?;
            if matches!(state.phase, KeyValueManagerPhase::Running) {
                return Ok(());
            }
            if matches!(state.phase, KeyValueManagerPhase::Starting) {
                return Err(SandboxError::provider(
                    "key-value startup already in progress",
                ));
            }
            state.phase = KeyValueManagerPhase::Starting;
            state.running = None;
        }

        match self.inner.service.start().await {
            Ok(running) => {
                let mut state = self.state()?;
                state.phase = KeyValueManagerPhase::Running;
                state.running = Some(running);
                Ok(())
            }
            Err(error) => {
                let mut state = self.state()?;
                state.phase = KeyValueManagerPhase::Failed {
                    message: error.to_string(),
                };
                state.running = None;
                Err(error)
            }
        }
    }

    /// Stop key-value storage if it is running.
    pub fn shutdown(&self) -> SandboxResult<()> {
        let running = {
            let mut state = self.state()?;
            state.phase = KeyValueManagerPhase::Stopped;
            state.running.take()
        };

        if let Some(running) = running {
            if let Err(error) = running.stop() {
                let mut state = self.state()?;
                state.phase = KeyValueManagerPhase::Failed {
                    message: error.to_string(),
                };
                return Err(error);
            }
        }

        Ok(())
    }

    /// Return the current key-value service profile.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match &state.phase {
            KeyValueManagerPhase::NotStarted => {
                self.inner.service.config.profile(&unknown_health())
            }
            KeyValueManagerPhase::Starting => self.inner.service.config.profile(&starting_health()),
            KeyValueManagerPhase::Running => state.running.as_ref().map_or_else(
                || {
                    self.inner.service.config.profile(&failed_health(
                        "key-value running state is missing runtime handle",
                    ))
                },
                RunningKeyValue::profile,
            ),
            KeyValueManagerPhase::Failed { message } => {
                self.inner.service.config.profile(&failed_health(message))
            }
            KeyValueManagerPhase::Stopped => self.inner.service.config.profile(&stopped_health()),
        }
    }

    /// Return the current key-value lifecycle state.
    #[must_use]
    pub fn phase(&self) -> KeyValueManagerPhase {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase
            .clone()
    }

    fn state(&self) -> SandboxResult<std::sync::MutexGuard<'_, KeyValueManagerRecord>> {
        self.inner
            .state
            .lock()
            .map_err(|_| SandboxError::provider("key-value manager lock poisoned"))
    }
}

/// Local key-value lifecycle boundary.
#[derive(Clone)]
pub struct KeyValueService {
    config: KeyValueConfig,
    runner: Arc<dyn EmbeddedKeyValueRunner>,
    health_probe: Arc<dyn KeyValueHealthProbe>,
}

impl KeyValueService {
    /// Create a key-value service with production seams.
    #[must_use]
    pub fn new(config: KeyValueConfig) -> Self {
        Self::with_components(
            config,
            Arc::new(TokioEmbeddedKeyValueRunner::default()),
            Arc::new(TcpKeyValueHealthProbe),
        )
    }

    /// Create a key-value service from fakeable lifecycle seams.
    #[must_use]
    pub fn with_components(
        config: KeyValueConfig,
        runner: Arc<dyn EmbeddedKeyValueRunner>,
        health_probe: Arc<dyn KeyValueHealthProbe>,
    ) -> Self {
        Self {
            config,
            runner,
            health_probe,
        }
    }

    /// Return the configured service profile without checking readiness.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        key_value_profile(&self.config)
    }

    /// Start key-value storage and wait for the configured health probe.
    pub async fn start(&self) -> SandboxResult<RunningKeyValue> {
        std::fs::create_dir_all(&self.config.data_dir).map_err(|error| {
            SandboxError::provider(format!(
                "create key-value data dir {}: {error}",
                self.config.data_dir.display()
            ))
        })?;
        let handle = self.runner.start(&self.config)?;
        if let Err(error) = self.health_probe.wait_until_ready(&self.config).await {
            let _ = self.runner.stop(&handle);
            return Err(error);
        }

        Ok(RunningKeyValue {
            config: self.config.clone(),
            handle,
            runner: Arc::clone(&self.runner),
        })
    }
}

/// Running key-value service handle.
#[derive(Clone)]
pub struct RunningKeyValue {
    config: KeyValueConfig,
    handle: EmbeddedKeyValueHandle,
    runner: Arc<dyn EmbeddedKeyValueRunner>,
}

impl RunningKeyValue {
    /// Return the ready service profile.
    #[must_use]
    pub fn profile(&self) -> LocalServiceProfile {
        self.config.profile(&ready_health())
    }

    /// Stop the running key-value backend.
    pub fn stop(&self) -> SandboxResult<()> {
        self.runner.stop(&self.handle)
    }
}

/// Fakeable embedded key-value runner.
pub trait EmbeddedKeyValueRunner: Send + Sync {
    /// Start embedded key-value storage and return a handle.
    fn start(&self, config: &KeyValueConfig) -> SandboxResult<EmbeddedKeyValueHandle>;

    /// Stop embedded key-value storage.
    fn stop(&self, handle: &EmbeddedKeyValueHandle) -> SandboxResult<()>;
}

/// Handle returned by an embedded key-value runner.
#[derive(Clone)]
pub struct EmbeddedKeyValueHandle {
    /// Runner-local handle identifier.
    pub id: String,
}

struct EmbeddedKeyValueServer {
    shutdown: broadcast::Sender<()>,
    task: JoinHandle<()>,
}

/// Fakeable key-value health probe.
pub trait KeyValueHealthProbe: Send + Sync {
    /// Wait until key-value storage is reachable.
    fn wait_until_ready<'a>(
        &'a self,
        config: &'a KeyValueConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SandboxResult<()>> + Send + 'a>>;
}

/// Run embedded Redis-compatible key-value storage with patched `tinyredis`.
#[derive(Default)]
pub struct TokioEmbeddedKeyValueRunner {
    servers: Mutex<BTreeMap<String, EmbeddedKeyValueServer>>,
}

impl EmbeddedKeyValueRunner for TokioEmbeddedKeyValueRunner {
    fn start(&self, config: &KeyValueConfig) -> SandboxResult<EmbeddedKeyValueHandle> {
        match config.backend {
            KeyValueBackendKind::Tinyredis => self.start_tinyredis(config),
        }
    }

    fn stop(&self, handle: &EmbeddedKeyValueHandle) -> SandboxResult<()> {
        let server = self
            .servers
            .lock()
            .map_err(|_| SandboxError::provider("embedded key-value registry lock poisoned"))?
            .remove(&handle.id)
            .ok_or_else(|| SandboxError::provider("embedded key-value handle is unknown"))?;
        let _ = server.shutdown.send(());
        tokio::spawn(async move {
            if let Err(error) = server.task.await {
                if !error.is_cancelled() {
                    tracing::warn!(error = %error, "embedded key-value task failed during shutdown");
                }
            }
        });
        Ok(())
    }
}

impl TokioEmbeddedKeyValueRunner {
    fn start_tinyredis(&self, config: &KeyValueConfig) -> SandboxResult<EmbeddedKeyValueHandle> {
        let bind_host = config.host.parse::<IpAddr>().map_err(|error| {
            SandboxError::invalid_request(format!("key-value host must be an IP address: {error}"))
        })?;
        let std_listener =
            std::net::TcpListener::bind((bind_host, config.port)).map_err(|error| {
                SandboxError::provider(format!(
                    "bind embedded key-value {}:{}: {error}",
                    config.host, config.port
                ))
            })?;
        std_listener.set_nonblocking(true).map_err(|error| {
            SandboxError::provider(format!("configure key-value listener: {error}"))
        })?;
        let listener = TcpListener::from_std(std_listener).map_err(|error| {
            SandboxError::provider(format!("create key-value listener: {error}"))
        })?;

        let tinyredis_config = TinyredisConfig {
            bind: config.host.clone(),
            port: config.port,
            appendonly: false,
            appendfilename: config.data_dir.join("tinyredis.aof"),
            requirepass: config.password.clone(),
            ..TinyredisConfig::default()
        };
        let store = Arc::new(TokioMutex::new(Store::new()));
        let (shutdown, _) = broadcast::channel::<()>(1);
        let stats = Arc::new(ServerStats::from_config(
            &tinyredis_config,
            String::new(),
            None,
        ));
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            server::serve(listener, store, None, task_shutdown, stats).await;
        });
        let id = format!("embedded-tinyredis-{}", config.port);

        self.servers
            .lock()
            .map_err(|_| SandboxError::provider("embedded key-value registry lock poisoned"))?
            .insert(id.clone(), EmbeddedKeyValueServer { shutdown, task });

        Ok(EmbeddedKeyValueHandle { id })
    }
}

/// TCP readiness probe for the Redis-compatible listener.
#[derive(Default)]
pub struct TcpKeyValueHealthProbe;

impl KeyValueHealthProbe for TcpKeyValueHealthProbe {
    fn wait_until_ready<'a>(
        &'a self,
        config: &'a KeyValueConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SandboxResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            loop {
                if TcpStream::connect((config.probe_host(), config.port))
                    .await
                    .is_ok()
                {
                    return Ok(());
                }

                if started.elapsed() >= config.readiness_timeout {
                    return Err(SandboxError::provider(format!(
                        "key-value did not become ready on {}:{}",
                        config.probe_host(),
                        config.port
                    )));
                }

                sleep(Duration::from_millis(100)).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use local_sandbox_core::LocalHealthStatus;
    use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Mutex};

    #[test]
    fn key_value_config_reads_product_neutral_environment() {
        let env = BTreeMap::from([
            (ENV_KEY_VALUE_DATA_DIR, "/var/local-sandbox/key-value-data"),
            (ENV_KEY_VALUE_HOST, "0.0.0.0"),
            (ENV_KEY_VALUE_PORT, "16380"),
            (ENV_KEY_VALUE_GUEST_HOST, "host.internal"),
            (ENV_KEY_VALUE_PASSWORD, "test-password"),
        ]);

        let config = KeyValueConfig::from_env_reader(|name| {
            env.get(name)
                .map(|value| OsString::from((*value).to_string()))
        })
        .unwrap();

        assert_eq!(config.backend, KeyValueBackendKind::Tinyredis);
        assert_eq!(
            config.data_dir,
            PathBuf::from("/var/local-sandbox/key-value-data")
        );
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 16_380);
        assert_eq!(config.host_endpoint(), "redis://127.0.0.1:16380");
        assert_eq!(config.guest_endpoint(), "redis://host.internal:16380");
        assert_eq!(config.password.as_deref(), Some("test-password"));
    }

    #[test]
    fn key_value_config_rejects_invalid_ports() {
        let result = KeyValueConfig::from_env_reader(|name| {
            (name == ENV_KEY_VALUE_PORT).then(|| OsString::from("not-a-port"))
        });

        assert!(matches!(
            result,
            Err(SandboxError::InvalidRequest { message }) if message.contains(ENV_KEY_VALUE_PORT)
        ));
    }

    #[test]
    fn key_value_config_rejects_non_ip_bind_hosts() {
        let result = KeyValueConfig::from_env_reader(|name| {
            (name == ENV_KEY_VALUE_HOST).then(|| OsString::from("localhost"))
        });

        assert!(matches!(
            result,
            Err(SandboxError::InvalidRequest { message })
                if message.contains(ENV_KEY_VALUE_HOST) && message.contains("IP address")
        ));
    }

    #[test]
    fn key_value_profile_contains_redis_facts_and_bounded_capabilities() {
        let config = KeyValueConfig {
            data_dir: PathBuf::from("/tmp/local-sandbox/key-value-data"),
            host_endpoint_host: "127.0.0.1".to_string(),
            guest_endpoint_host: "host.internal".to_string(),
            password: Some("test-password".to_string()),
            ..KeyValueConfig::default()
        };

        let profile = key_value_profile(&config);

        assert_eq!(profile.health.status, LocalHealthStatus::Unknown);
        assert_eq!(profile.endpoints.len(), 2);
        assert!(profile.endpoints.iter().any(|endpoint| {
            endpoint.name == "host_redis"
                && endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::KeyValue
                && endpoint.protocol == LocalEndpointProtocol::Redis
                && endpoint.url.as_deref() == Some("redis://127.0.0.1:16379")
        }));
        assert!(profile.endpoints.iter().any(|endpoint| {
            endpoint.name == "guest_redis"
                && endpoint.visibility == LocalEndpointVisibility::Guest
                && endpoint.url.as_deref() == Some("redis://host.internal:16379")
        }));
        assert_eq!(
            profile.credentials.as_ref().unwrap().password.as_deref(),
            Some("test-password")
        );
        assert!(profile.object_storage.is_none());

        let key_value = profile.key_value.as_ref().unwrap();
        assert_eq!(key_value.database, 0);
        assert_eq!(key_value.implementation, "tinyredis");
        assert!(key_value.capabilities.ping);
        assert!(key_value.capabilities.strings);
        assert!(key_value.capabilities.expiry);
        assert!(key_value.capabilities.hashes);
        assert!(key_value.capabilities.lists);
        assert!(key_value.capabilities.sets);
        assert!(key_value.capabilities.sorted_sets);
        assert!(key_value.capabilities.scan);
        assert!(key_value.capabilities.pipeline);
        assert!(key_value.capabilities.transactions);
        assert!(key_value.capabilities.targeted_lock_scripts);
        assert!(!key_value.capabilities.lua_scripts);
    }

    #[tokio::test]
    async fn key_value_start_uses_fakeable_seams_and_returns_ready_profile() {
        let config = KeyValueConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-key-value-test-{}",
                std::process::id()
            )),
            readiness_timeout: Duration::from_millis(1),
            ..KeyValueConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service =
            KeyValueService::with_components(config.clone(), runner.clone(), Arc::new(FakeProbe));

        let running = service.start().await.unwrap();

        assert_eq!(
            runner.started.lock().unwrap().as_slice(),
            std::slice::from_ref(&config.data_dir)
        );
        assert_eq!(running.profile().health.status, LocalHealthStatus::Ready);
        running.stop().unwrap();
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-tinyredis".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn key_value_start_stops_runner_when_health_fails() {
        let config = KeyValueConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-key-value-test-fail-{}",
                std::process::id()
            )),
            readiness_timeout: Duration::from_millis(1),
            ..KeyValueConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service = KeyValueService::with_components(
            config.clone(),
            runner.clone(),
            Arc::new(FailingProbe),
        );

        let result = service.start().await;

        assert!(
            matches!(result, Err(SandboxError::Provider { message }) if message == "health failed")
        );
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-tinyredis".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn key_value_manager_stores_running_service_and_stops_on_shutdown() {
        let config = KeyValueConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-key-value-manager-test-{}",
                std::process::id()
            )),
            readiness_timeout: Duration::from_millis(1),
            ..KeyValueConfig::default()
        };
        let runner = Arc::new(FakeRunner::default());
        let service =
            KeyValueService::with_components(config.clone(), runner.clone(), Arc::new(FakeProbe));
        let manager = KeyValueManager::new(service);

        assert_eq!(manager.profile().health.status, LocalHealthStatus::Unknown);
        manager.start().await.unwrap();
        assert_eq!(manager.profile().health.status, LocalHealthStatus::Ready);
        manager.shutdown().unwrap();

        assert_eq!(manager.profile().health.status, LocalHealthStatus::Stopped);
        assert_eq!(
            runner.stopped.lock().unwrap().as_slice(),
            &["fake-tinyredis".to_string()]
        );
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn key_value_manager_keeps_failed_profile_after_start_failure() {
        let config = KeyValueConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-key-value-manager-fail-test-{}",
                std::process::id()
            )),
            readiness_timeout: Duration::from_millis(1),
            ..KeyValueConfig::default()
        };
        let service = KeyValueService::with_components(
            config.clone(),
            Arc::new(FakeRunner::default()),
            Arc::new(FailingProbe),
        );
        let manager = KeyValueManager::new(service);

        let result = manager.start().await;

        assert!(result.is_err());
        let profile = manager.profile();
        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert!(profile.health.message.unwrap().contains("health failed"));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn key_value_manager_rejects_concurrent_startup() {
        let config = KeyValueConfig {
            data_dir: env::temp_dir().join(format!(
                "local-sandbox-key-value-manager-slow-test-{}",
                std::process::id()
            )),
            readiness_timeout: Duration::from_millis(1),
            ..KeyValueConfig::default()
        };
        let service = KeyValueService::with_components(
            config.clone(),
            Arc::new(FakeRunner::default()),
            Arc::new(SlowProbe),
        );
        let manager = KeyValueManager::new(service);
        let first_start_manager = manager.clone();
        let first_start = tokio::spawn(async move { first_start_manager.start().await });

        while !matches!(manager.phase(), KeyValueManagerPhase::Starting) {
            tokio::task::yield_now().await;
        }

        let second_start = manager.start().await;

        assert!(
            matches!(second_start, Err(SandboxError::Provider { message }) if message == "key-value startup already in progress")
        );
        first_start.await.unwrap().unwrap();
        manager.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[test]
    fn key_value_manager_reports_missing_running_handle_as_failed() {
        let manager = KeyValueManager::from_config(KeyValueConfig::default());
        manager.set_phase_for_test(KeyValueManagerPhase::Running);

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert_eq!(
            profile.health.message.as_deref(),
            Some("key-value running state is missing runtime handle")
        );
    }

    #[test]
    fn key_value_manager_failed_constructor_profiles_error() {
        let manager = KeyValueManager::failed(
            KeyValueConfig::default(),
            "invalid key-value config".to_string(),
        );

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Failed);
        assert_eq!(
            profile.health.message.as_deref(),
            Some("invalid key-value config")
        );
    }

    #[test]
    fn key_value_manager_profiles_starting_state() {
        let manager = KeyValueManager::from_config(KeyValueConfig::default());
        manager.set_phase_for_test(KeyValueManagerPhase::Starting);

        let profile = manager.profile();

        assert_eq!(profile.health.status, LocalHealthStatus::Starting);
        assert_eq!(profile.health.message.as_deref(), Some("starting"));
    }

    impl KeyValueManager {
        fn set_phase_for_test(&self, phase: KeyValueManagerPhase) {
            self.inner.state.lock().unwrap().phase = phase;
        }
    }

    #[derive(Default)]
    struct FakeRunner {
        started: Mutex<Vec<PathBuf>>,
        stopped: Mutex<Vec<String>>,
    }

    impl EmbeddedKeyValueRunner for FakeRunner {
        fn start(&self, config: &KeyValueConfig) -> SandboxResult<EmbeddedKeyValueHandle> {
            self.started.lock().unwrap().push(config.data_dir.clone());
            Ok(EmbeddedKeyValueHandle {
                id: "fake-tinyredis".to_string(),
            })
        }

        fn stop(&self, handle: &EmbeddedKeyValueHandle) -> SandboxResult<()> {
            self.stopped.lock().unwrap().push(handle.id.clone());
            Ok(())
        }
    }

    struct FakeProbe;

    impl KeyValueHealthProbe for FakeProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a KeyValueConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FailingProbe;

    impl KeyValueHealthProbe for FailingProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a KeyValueConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async { Err(SandboxError::provider("health failed")) })
        }
    }

    struct SlowProbe;

    impl KeyValueHealthProbe for SlowProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a KeyValueConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async {
                tokio::task::yield_now().await;
                Ok(())
            })
        }
    }
}
