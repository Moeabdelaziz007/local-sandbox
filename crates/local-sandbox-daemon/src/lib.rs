//! HTTP daemon for local sandbox clients.

use axum::{
    body::Body,
    extract::{OriginalUri, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
    Json, Router,
};
use local_sandbox_core::{
    CommandId, CommandWait, CreateCommandRequest, CreateSandboxRequest, CreateSnapshotRequest,
    ExtendSandboxTimeoutRequest, InMemorySandboxProvider, LocalCredentials, LocalDaemonProfile,
    LocalEndpoint, LocalEndpointProtocol, LocalEndpointPurpose, LocalEndpointVisibility,
    LocalHealth, LocalHealthStatus, LocalProfile, LocalRouteProfile, LocalSandboxProfile,
    LocalServiceProfile, SandboxError, SandboxErrorCode, SandboxId, SandboxInfo, SandboxProvider,
    SandboxResult, SandboxRoute, SandboxRouteProtocol, SetNetworkPolicyRequest, WriteFilesRequest,
};
use local_sandbox_services::{
    KeyValueConfig, KeyValueManager, ObjectStorageConfig, ObjectStorageManager,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

const PROXY_REQUEST_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const PROXY_RESPONSE_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const PROXY_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Build daemon state for the production daemon process.
pub async fn boot_state() -> SandboxResult<DaemonState> {
    let object_storage = match ObjectStorageConfig::from_env() {
        Ok(config) => ObjectStorageManager::from_config(config),
        Err(error) => {
            tracing::warn!(error = %error, "invalid object-storage configuration; daemon will keep serving profile");
            ObjectStorageManager::failed(ObjectStorageConfig::default(), error.to_string())
        }
    };
    let key_value = match KeyValueConfig::from_env() {
        Ok(config) => KeyValueManager::from_config(config),
        Err(error) => {
            tracing::warn!(error = %error, "invalid key-value configuration; daemon will keep serving profile");
            KeyValueManager::failed(KeyValueConfig::default(), error.to_string())
        }
    };

    if let Err(error) = object_storage.start().await {
        tracing::warn!(error = %error, "object storage failed to start; daemon will keep serving profile");
    }
    if let Err(error) = key_value.start().await {
        tracing::warn!(error = %error, "key-value storage failed to start; daemon will keep serving profile");
    }

    Ok(DaemonState::with_services(
        InMemorySandboxProvider::new(),
        object_storage,
        key_value,
    ))
}

/// Build daemon state with an explicit object-storage config.
pub async fn boot_state_with_object_storage_config(
    config: ObjectStorageConfig,
) -> SandboxResult<DaemonState> {
    let object_storage = ObjectStorageManager::from_config(config);
    if let Err(error) = object_storage.start().await {
        tracing::warn!(error = %error, "object storage failed to start; daemon will keep serving profile");
    }

    Ok(DaemonState::with_services(
        InMemorySandboxProvider::new(),
        object_storage,
        KeyValueManager::from_config(KeyValueConfig::default()),
    ))
}

/// Build daemon state with an explicit key-value config.
pub async fn boot_state_with_key_value_config(
    config: KeyValueConfig,
) -> SandboxResult<DaemonState> {
    let key_value = KeyValueManager::from_config(config);
    if let Err(error) = key_value.start().await {
        tracing::warn!(error = %error, "key-value storage failed to start; daemon will keep serving profile");
    }

    Ok(DaemonState::with_services(
        InMemorySandboxProvider::new(),
        ObjectStorageManager::from_config(ObjectStorageConfig::default()),
        key_value,
    ))
}

/// Serve the daemon until the shutdown future resolves, then stop owned services.
pub async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    state: DaemonState,
    shutdown: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let object_storage = state.object_storage_manager();
    let key_value = state.key_value_manager();
    let result = axum::serve(listener, router_with_state(state))
        .with_graceful_shutdown(shutdown)
        .await;

    if let Err(error) = object_storage.shutdown() {
        tracing::warn!(error = %error, "object storage shutdown failed");
    }
    if let Err(error) = key_value.shutdown() {
        tracing::warn!(error = %error, "key-value shutdown failed");
    }

    result
}

/// Shared daemon application state.
#[derive(Clone)]
pub struct DaemonState {
    provider: Arc<dyn SandboxProvider>,
    proxy_client: reqwest::Client,
    profile_source: Arc<dyn ServiceProfileSource>,
    /// Volatile route facts recorded after successful create and removed after successful delete.
    /// This is not reconciled from durable provider state yet.
    route_profiles: Arc<Mutex<BTreeMap<SandboxId, Vec<LocalRouteProfile>>>>,
    object_storage: ObjectStorageManager,
    key_value: KeyValueManager,
}

impl DaemonState {
    /// Create daemon state from a sandbox provider.
    #[must_use]
    pub fn new(provider: impl SandboxProvider) -> Self {
        let object_storage = ObjectStorageManager::from_config(ObjectStorageConfig::default());
        let key_value = KeyValueManager::from_config(KeyValueConfig::default());
        Self {
            provider: Arc::new(provider),
            proxy_client: reqwest::Client::new(),
            profile_source: Arc::new(DefaultServiceProfileSource::new(
                object_storage.clone(),
                key_value.clone(),
            )),
            route_profiles: Arc::new(Mutex::new(BTreeMap::new())),
            object_storage,
            key_value,
        }
    }

    /// Create daemon state with the daemon-owned object-storage manager.
    #[must_use]
    pub fn with_object_storage(
        provider: impl SandboxProvider,
        object_storage: ObjectStorageManager,
    ) -> Self {
        Self::with_services(
            provider,
            object_storage,
            KeyValueManager::from_config(KeyValueConfig::default()),
        )
    }

    /// Create daemon state with daemon-owned service managers.
    #[must_use]
    pub fn with_services(
        provider: impl SandboxProvider,
        object_storage: ObjectStorageManager,
        key_value: KeyValueManager,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            proxy_client: reqwest::Client::new(),
            profile_source: Arc::new(DefaultServiceProfileSource::new(
                object_storage.clone(),
                key_value.clone(),
            )),
            route_profiles: Arc::new(Mutex::new(BTreeMap::new())),
            object_storage,
            key_value,
        }
    }

    /// Override service profile facts for tests or alternate daemon wiring.
    #[must_use]
    pub fn with_profile_source(
        mut self,
        profile_source: impl ServiceProfileSource + 'static,
    ) -> Self {
        self.profile_source = Arc::new(profile_source);
        self
    }

    /// Return the daemon-owned object-storage manager.
    #[must_use]
    pub fn object_storage_manager(&self) -> ObjectStorageManager {
        self.object_storage.clone()
    }

    /// Return the daemon-owned key-value manager.
    #[must_use]
    pub fn key_value_manager(&self) -> KeyValueManager {
        self.key_value.clone()
    }

    async fn profile(&self, daemon_base_url: Option<&str>) -> Result<LocalProfile, SandboxError> {
        let capabilities = self.provider.capabilities().await?;
        let routes = self.route_profiles(daemon_base_url);
        Ok(default_profile(
            capabilities.provider,
            routes,
            self.profile_source.services(),
        ))
    }

    async fn create_sandbox(
        &self,
        request: CreateSandboxRequest,
    ) -> Result<SandboxInfo, SandboxError> {
        validate_unique_route_names(&request)?;
        let sandbox = self.provider.create_sandbox(request).await?;
        self.record_route_profiles(&sandbox);
        Ok(sandbox)
    }

    async fn delete_sandbox(&self, sandbox_id: SandboxId) -> Result<SandboxInfo, SandboxError> {
        let sandbox = self.provider.delete_sandbox(sandbox_id).await?;
        self.remove_route_profiles(sandbox_id);
        Ok(sandbox)
    }

    fn route_profiles(&self, daemon_base_url: Option<&str>) -> Vec<LocalRouteProfile> {
        self.route_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .flat_map(|profiles| {
                profiles
                    .iter()
                    .map(|profile| route_profile_with_daemon_base_url(profile, daemon_base_url))
            })
            .collect()
    }

    fn route_profile(
        &self,
        sandbox_id: SandboxId,
        route_name: &str,
    ) -> Result<LocalRouteProfile, ProxyError> {
        let route_profiles = self
            .route_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let profiles = route_profiles
            .get(&sandbox_id)
            .ok_or_else(|| ProxyError::sandbox_not_found(sandbox_id))?;
        profiles
            .iter()
            .find(|profile| profile.name == route_name)
            .cloned()
            .ok_or_else(|| ProxyError::route_not_found(sandbox_id, route_name))
    }

    fn record_route_profiles(&self, sandbox: &SandboxInfo) {
        let profiles = route_profiles_from_routes(sandbox.id, &sandbox.routes);
        let mut route_profiles = self
            .route_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if profiles.is_empty() {
            route_profiles.remove(&sandbox.id);
        } else {
            route_profiles.insert(sandbox.id, profiles);
        }
    }

    fn remove_route_profiles(&self, sandbox_id: SandboxId) {
        self.route_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&sandbox_id);
    }
}

/// Source of product-neutral local service profile facts.
pub trait ServiceProfileSource: Send + Sync {
    /// Return local service profiles keyed by product-neutral service name.
    fn services(&self) -> BTreeMap<String, LocalServiceProfile>;
}

struct DefaultServiceProfileSource {
    object_storage: ObjectStorageManager,
    key_value: KeyValueManager,
}

impl DefaultServiceProfileSource {
    fn new(object_storage: ObjectStorageManager, key_value: KeyValueManager) -> Self {
        Self {
            object_storage,
            key_value,
        }
    }
}

impl ServiceProfileSource for DefaultServiceProfileSource {
    fn services(&self) -> BTreeMap<String, LocalServiceProfile> {
        let mut services = default_unknown_services();
        services.insert("object_storage".to_string(), self.object_storage.profile());
        services.insert("key_value".to_string(), self.key_value.profile());
        services
    }
}

/// Build the daemon router using the default in-memory provider.
pub fn router() -> Router {
    router_with_state(DaemonState::new(InMemorySandboxProvider::new()))
}

/// Build the daemon router with injected provider state.
pub fn router_with_state(state: DaemonState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/profile", get(profile))
        .route("/capabilities", get(capabilities))
        .route("/sandboxes", post(create_sandbox))
        .route(
            "/sandboxes/{sandbox_id}",
            get(get_sandbox).delete(delete_sandbox),
        )
        .route("/sandboxes/{sandbox_id}/commands", post(create_command))
        .route(
            "/sandboxes/{sandbox_id}/commands/{command_id}",
            get(get_command).delete(cancel_command),
        )
        .route("/sandboxes/{sandbox_id}/files", post(write_files))
        .route("/sandboxes/{sandbox_id}/timeout", post(extend_timeout))
        .route(
            "/sandboxes/{sandbox_id}/network-policy",
            put(set_network_policy),
        )
        .route("/sandboxes/{sandbox_id}/snapshots", post(create_snapshot))
        .route("/proxy/routes/{sandbox_id}/{route_name}", any(proxy_route))
        .route(
            "/proxy/routes/{sandbox_id}/{route_name}/{*tail}",
            any(proxy_route),
        )
        .with_state(state)
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Deserialize)]
struct CommandQuery {
    #[serde(default)]
    wait: bool,
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
struct ErrorResponse {
    code: SandboxErrorCode,
    message: String,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn profile(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, DaemonError> {
    let daemon_base_url = daemon_http_base_url(&headers);
    Ok(Json(state.profile(daemon_base_url.as_deref()).await?))
}

async fn capabilities(State(state): State<DaemonState>) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(state.provider.capabilities().await?))
}

async fn create_sandbox(
    State(state): State<DaemonState>,
    Json(request): Json<CreateSandboxRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok((
        StatusCode::CREATED,
        Json(state.create_sandbox(request).await?),
    ))
}

async fn get_sandbox(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(state.provider.get_sandbox(sandbox_id).await?))
}

async fn delete_sandbox(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(state.delete_sandbox(sandbox_id).await?))
}

async fn create_command(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
    Json(request): Json<CreateCommandRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok((
        StatusCode::CREATED,
        Json(state.provider.create_command(sandbox_id, request).await?),
    ))
}

async fn get_command(
    State(state): State<DaemonState>,
    Path((sandbox_id, command_id)): Path<(SandboxId, CommandId)>,
    Query(query): Query<CommandQuery>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(
        state
            .provider
            .get_command(
                sandbox_id,
                command_id,
                CommandWait {
                    wait: query.wait,
                    timeout_ms: query.timeout_ms,
                },
            )
            .await?,
    ))
}

async fn cancel_command(
    State(state): State<DaemonState>,
    Path((sandbox_id, command_id)): Path<(SandboxId, CommandId)>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(
        state
            .provider
            .cancel_command(sandbox_id, command_id)
            .await?,
    ))
}

async fn write_files(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
    Json(request): Json<WriteFilesRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(state.provider.write_files(sandbox_id, request).await?))
}

async fn extend_timeout(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
    Json(request): Json<ExtendSandboxTimeoutRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(
        state.provider.extend_timeout(sandbox_id, request).await?,
    ))
}

async fn set_network_policy(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
    Json(request): Json<SetNetworkPolicyRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    state
        .provider
        .set_network_policy(sandbox_id, request)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_snapshot(
    State(state): State<DaemonState>,
    Path(sandbox_id): Path<SandboxId>,
    Json(request): Json<CreateSnapshotRequest>,
) -> Result<impl IntoResponse, DaemonError> {
    Ok((
        StatusCode::CREATED,
        Json(state.provider.create_snapshot(sandbox_id, request).await?),
    ))
}

#[derive(Deserialize)]
struct ProxyRoutePath {
    sandbox_id: SandboxId,
    route_name: String,
    #[serde(default, rename = "tail")]
    _tail: Option<String>,
}

async fn proxy_route(
    State(state): State<DaemonState>,
    Path(path): Path<ProxyRoutePath>,
    OriginalUri(original_uri): OriginalUri,
    method: Method,
    body: Body,
) -> Result<impl IntoResponse, ProxyError> {
    let route = state.route_profile(path.sandbox_id, &path.route_name)?;
    let target_url = route_target_http_url(&route)
        .ok_or_else(|| ProxyError::unsupported(path.sandbox_id, &path.route_name))?;
    let tail = encoded_proxy_tail(original_uri.path());
    let upstream_url = upstream_proxy_url(&target_url, tail, original_uri.query());
    let body = axum::body::to_bytes(body, PROXY_REQUEST_BODY_LIMIT_BYTES)
        .await
        .map_err(|error| {
            ProxyError::provider(format!("failed to read proxy request body: {error}"))
        })?;
    let method = reqwest::Method::from_bytes(method.as_str().as_bytes()).map_err(|error| {
        ProxyError::provider(format!("failed to forward proxy request method: {error}"))
    })?;
    let upstream = state
        .proxy_client
        .request(method, upstream_url)
        .timeout(PROXY_UPSTREAM_TIMEOUT)
        .body(body)
        .send()
        .await
        .map_err(|error| ProxyError::provider(format!("upstream request failed: {error}")))?;
    let status = StatusCode::from_u16(upstream.status().as_u16()).map_err(|error| {
        ProxyError::provider(format!("upstream returned invalid status: {error}"))
    })?;
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| HeaderValue::from_str(value).ok());
    let body = read_limited_upstream_body(upstream).await?;
    let mut response = (status, body).into_response();
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    Ok(response)
}

struct ProxyError {
    status: StatusCode,
    code: SandboxErrorCode,
    message: String,
}

impl ProxyError {
    fn sandbox_not_found(sandbox_id: SandboxId) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: SandboxErrorCode::SandboxNotFound,
            message: format!("sandbox not found: {sandbox_id}"),
        }
    }

    fn route_not_found(sandbox_id: SandboxId, route_name: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: SandboxErrorCode::RouteNotFound,
            message: format!("route not found: {route_name} in sandbox {sandbox_id}"),
        }
    }

    fn unsupported(sandbox_id: SandboxId, route_name: &str) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: SandboxErrorCode::Unsupported,
            message: format!(
                "unsupported proxy route protocol: {route_name} in sandbox {sandbox_id}"
            ),
        }
    }

    fn provider(message: String) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: SandboxErrorCode::Provider,
            message,
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

struct DaemonError(SandboxError);

impl From<SandboxError> for DaemonError {
    fn from(value: SandboxError) -> Self {
        Self(value)
    }
}

impl IntoResponse for DaemonError {
    fn into_response(self) -> Response {
        let code = self.0.code();
        let status = match self.0.code() {
            SandboxErrorCode::SandboxNotFound
            | SandboxErrorCode::RouteNotFound
            | SandboxErrorCode::CommandNotFound => StatusCode::NOT_FOUND,
            SandboxErrorCode::Unsupported => StatusCode::NOT_IMPLEMENTED,
            SandboxErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
            SandboxErrorCode::Conflict => StatusCode::CONFLICT,
            SandboxErrorCode::Provider => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorResponse {
                code,
                message: error_message(&self.0),
            }),
        )
            .into_response()
    }
}

fn error_message(error: &SandboxError) -> String {
    if error.code() == SandboxErrorCode::Provider {
        tracing::error!(error = %error, "provider error while handling daemon request");
        "provider error".to_string()
    } else {
        error.to_string()
    }
}

fn validate_unique_route_names(request: &CreateSandboxRequest) -> Result<(), SandboxError> {
    let mut names = BTreeSet::new();
    for route in &request.routes {
        if !names.insert(route.name.clone()) {
            return Err(SandboxError::invalid_request(format!(
                "duplicate route name: {}",
                route.name
            )));
        }
    }
    Ok(())
}

async fn read_limited_upstream_body(upstream: reqwest::Response) -> Result<Vec<u8>, ProxyError> {
    read_limited_upstream_body_with_limit(upstream, PROXY_RESPONSE_BODY_LIMIT_BYTES).await
}

async fn read_limited_upstream_body_with_limit(
    mut upstream: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ProxyError> {
    let mut body = Vec::new();
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|error| ProxyError::provider(format!("failed to read upstream body: {error}")))?
    {
        if body.len() + chunk.len() > limit {
            return Err(ProxyError::provider(format!(
                "upstream response exceeded proxy body limit of {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn default_profile(
    provider: String,
    routes: Vec<LocalRouteProfile>,
    services: BTreeMap<String, LocalServiceProfile>,
) -> LocalProfile {
    LocalProfile {
        daemon: LocalDaemonProfile {
            health: ready_health(),
            endpoints: vec![LocalEndpoint {
                name: "daemon_api".to_string(),
                visibility: LocalEndpointVisibility::Host,
                purpose: LocalEndpointPurpose::DaemonApi,
                protocol: LocalEndpointProtocol::Http,
                url: None,
                health: ready_health(),
            }],
        },
        sandbox: LocalSandboxProfile {
            provider,
            health: unknown_health(),
            endpoints: Vec::new(),
        },
        routes,
        services,
    }
}

fn route_profiles_from_routes(
    sandbox_id: SandboxId,
    routes: &[SandboxRoute],
) -> Vec<LocalRouteProfile> {
    routes
        .iter()
        .map(|route| route_profile_from_route(sandbox_id, route))
        .collect()
}

fn route_profile_from_route(sandbox_id: SandboxId, route: &SandboxRoute) -> LocalRouteProfile {
    LocalRouteProfile {
        name: route.name.clone(),
        sandbox_id: Some(sandbox_id),
        target_port: Some(route.target_port),
        health: unknown_health(),
        endpoints: route_endpoints_from_route(route),
    }
}

fn route_profile_with_daemon_base_url(
    profile: &LocalRouteProfile,
    daemon_base_url: Option<&str>,
) -> LocalRouteProfile {
    let mut profile = profile.clone();
    if let Some(daemon_base_url) = daemon_base_url {
        if let Some(sandbox_id) = profile.sandbox_id {
            for endpoint in &mut profile.endpoints {
                if endpoint.visibility == LocalEndpointVisibility::Browser
                    && endpoint.protocol == LocalEndpointProtocol::Http
                {
                    endpoint.url = Some(proxy_route_http_url(
                        daemon_base_url,
                        sandbox_id,
                        &profile.name,
                    ));
                }
                if endpoint.visibility == LocalEndpointVisibility::Browser
                    && endpoint.protocol == LocalEndpointProtocol::Ws
                {
                    endpoint.url = Some(proxy_route_ws_url(
                        daemon_base_url,
                        sandbox_id,
                        &profile.name,
                    ));
                }
            }
        }
    }
    profile
}

fn route_endpoints_from_route(route: &SandboxRoute) -> Vec<LocalEndpoint> {
    let mut endpoints = vec![host_route_endpoint(route)];

    if route.protocol == SandboxRouteProtocol::Http {
        endpoints.push(browser_route_endpoint(route));
        endpoints.push(browser_websocket_endpoint(route));
    }

    endpoints
}

fn host_route_endpoint(route: &SandboxRoute) -> LocalEndpoint {
    LocalEndpoint {
        name: "host".to_string(),
        visibility: LocalEndpointVisibility::Host,
        purpose: LocalEndpointPurpose::SandboxRoute,
        protocol: route_endpoint_protocol(route.protocol),
        url: Some(route.url.clone()),
        health: unknown_health(),
    }
}

fn browser_route_endpoint(_route: &SandboxRoute) -> LocalEndpoint {
    LocalEndpoint {
        name: "browser".to_string(),
        visibility: LocalEndpointVisibility::Browser,
        purpose: LocalEndpointPurpose::SandboxRoute,
        protocol: LocalEndpointProtocol::Http,
        url: None,
        health: unknown_health(),
    }
}

fn browser_websocket_endpoint(_route: &SandboxRoute) -> LocalEndpoint {
    LocalEndpoint {
        name: "browser_websocket".to_string(),
        visibility: LocalEndpointVisibility::Browser,
        purpose: LocalEndpointPurpose::Websocket,
        protocol: LocalEndpointProtocol::Ws,
        url: None,
        health: unknown_health(),
    }
}

fn daemon_http_base_url(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    Some(format!("http://{host}"))
}

fn proxy_route_http_url(daemon_base_url: &str, sandbox_id: SandboxId, route_name: &str) -> String {
    format!(
        "{}/proxy/routes/{}/{}",
        daemon_base_url.trim_end_matches('/'),
        sandbox_id,
        percent_encode_path_segment(route_name)
    )
}

fn proxy_route_ws_url(daemon_base_url: &str, sandbox_id: SandboxId, route_name: &str) -> String {
    let http_url = proxy_route_http_url(daemon_base_url, sandbox_id, route_name);
    format!("ws://{}", http_url.trim_start_matches("http://"))
}

fn percent_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn route_target_http_url(route: &LocalRouteProfile) -> Option<String> {
    route
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.name == "host"
                && endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::SandboxRoute
        })
        .and_then(|endpoint| {
            if endpoint.protocol == LocalEndpointProtocol::Http {
                endpoint.url.clone()
            } else {
                None
            }
        })
}

fn upstream_proxy_url(target_url: &str, tail: Option<&str>, query: Option<&str>) -> String {
    let mut url = target_url.trim_end_matches('/').to_string();
    if let Some(tail) = tail.filter(|tail| !tail.is_empty()) {
        url.push('/');
        url.push_str(tail.trim_start_matches('/'));
    }
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn encoded_proxy_tail(path: &str) -> Option<&str> {
    let mut segments = path.splitn(6, '/');
    segments.next()?;
    segments.next()?;
    segments.next()?;
    segments.next()?;
    segments.next()?;
    segments.next().filter(|tail| !tail.is_empty())
}

fn route_endpoint_protocol(protocol: SandboxRouteProtocol) -> LocalEndpointProtocol {
    match protocol {
        SandboxRouteProtocol::Http => LocalEndpointProtocol::Http,
        SandboxRouteProtocol::Https => LocalEndpointProtocol::Https,
        SandboxRouteProtocol::Tcp => LocalEndpointProtocol::Tcp,
    }
}

fn default_unknown_services() -> BTreeMap<String, LocalServiceProfile> {
    BTreeMap::from([
        (
            "database".to_string(),
            LocalServiceProfile {
                health: unwired_health(),
                endpoints: vec![
                    fake_endpoint(
                        "host_postgres",
                        LocalEndpointVisibility::Host,
                        LocalEndpointPurpose::Database,
                        LocalEndpointProtocol::Postgres,
                    ),
                    fake_endpoint(
                        "guest_postgres",
                        LocalEndpointVisibility::Guest,
                        LocalEndpointPurpose::Database,
                        LocalEndpointProtocol::Postgres,
                    ),
                ],
                credentials: Some(empty_credentials()),
                logs: Vec::new(),
                object_storage: None,
                key_value: None,
            },
        ),
        (
            "callbacks".to_string(),
            LocalServiceProfile {
                health: unwired_health(),
                endpoints: vec![fake_endpoint(
                    "host_callback",
                    LocalEndpointVisibility::Host,
                    LocalEndpointPurpose::Callback,
                    LocalEndpointProtocol::Http,
                )],
                credentials: None,
                logs: Vec::new(),
                object_storage: None,
                key_value: None,
            },
        ),
    ])
}

fn empty_credentials() -> LocalCredentials {
    LocalCredentials {
        access_key_id: None,
        secret_access_key: None,
        username: None,
        password: None,
        token: None,
    }
}

fn fake_endpoint(
    name: &str,
    visibility: LocalEndpointVisibility,
    purpose: LocalEndpointPurpose,
    protocol: LocalEndpointProtocol,
) -> LocalEndpoint {
    LocalEndpoint {
        name: name.to_string(),
        visibility,
        purpose,
        protocol,
        url: None,
        health: unknown_health(),
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
        message: None,
        checked_at_unix_ms: None,
    }
}

fn unwired_health() -> LocalHealth {
    LocalHealth {
        status: LocalHealthStatus::Unknown,
        message: Some("not wired yet".to_string()),
        checked_at_unix_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, Uri},
    };
    use local_sandbox_core::{
        CommandInfo, ProviderFuture, SandboxCapabilities, SandboxInfo, SandboxStatus, SnapshotInfo,
        WriteFilesResponse,
    };
    use local_sandbox_services::{
        EmbeddedObjectStorageHandle, EmbeddedObjectStorageRunner, MinioBinaryResolver,
        MinioProcessHandle, MinioProcessRunner, ObjectStorageBackendKind, ObjectStorageHealthProbe,
        ObjectStorageService,
    };
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};
    use std::{future::Future, path::Path, path::PathBuf, pin::Pin, time::Duration};
    use tower::ServiceExt;

    const RESPONSE_BODY_LIMIT: usize = 1024 * 1024;

    #[tokio::test]
    async fn health_and_capabilities_routes_work() {
        let app = test_router();

        let response = app
            .clone()
            .oneshot(request("GET", "/health", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body::<Value>(response).await, json!({ "ok": true }));

        let response = app
            .oneshot(request("GET", "/capabilities", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let capabilities = json_body::<SandboxCapabilities>(response).await;
        assert_eq!(capabilities.provider, "in_memory");
    }

    #[tokio::test]
    async fn profile_route_returns_product_neutral_endpoint_taxonomy() {
        let app = test_router();

        let response = app.oneshot(request("GET", "/profile", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let profile = json_body::<LocalProfile>(response).await;

        assert_eq!(profile.daemon.health.status, LocalHealthStatus::Ready);
        assert_eq!(profile.sandbox.provider, "in_memory");
        assert_eq!(profile.sandbox.health.status, LocalHealthStatus::Unknown);
        assert!(profile.routes.is_empty());

        let object_storage = profile.services.get("object_storage").unwrap();
        assert_eq!(object_storage.health.status, LocalHealthStatus::Unknown);
        assert_eq!(object_storage.endpoints.len(), 2);
        assert!(object_storage.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::ObjectStorageApi
                && endpoint.protocol == LocalEndpointProtocol::Http
                && endpoint.url.as_deref() == Some("http://127.0.0.1:9000")
        }));
        assert!(object_storage.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Guest
                && endpoint.purpose == LocalEndpointPurpose::ObjectStorageApi
                && endpoint.protocol == LocalEndpointProtocol::Http
                && endpoint.url.as_deref() == Some("http://host.internal:9000")
        }));
        assert!(object_storage.credentials.is_some());
        let object_storage_details = object_storage.object_storage.as_ref().unwrap();
        assert_eq!(object_storage_details.region, "us-east-1");
        assert!(object_storage_details.force_path_style);
        assert!(object_storage_details.capabilities.create_bucket);
        assert!(object_storage_details.capabilities.put_object);
        assert!(object_storage_details.capabilities.get_object);
        assert!(object_storage_details.capabilities.head_object);
        assert!(object_storage_details.capabilities.delete_object);
        assert!(object_storage_details.capabilities.delete_objects);
        assert!(object_storage_details.capabilities.list_objects_v2);
        assert!(object_storage_details.capabilities.copy_object);
        assert!(object_storage_details.capabilities.presigned_get);
        assert!(object_storage_details.capabilities.presigned_put);
        assert!(!object_storage_details.capabilities.cors);
        assert!(!object_storage_details.capabilities.browser_upload);
        assert!(!object_storage_details.capabilities.versioning);
        assert!(!object_storage_details.capabilities.list_object_versions);
        assert_eq!(object_storage.logs.len(), 1);

        let key_value = profile.services.get("key_value").unwrap();
        assert_eq!(key_value.health.status, LocalHealthStatus::Unknown);
        assert_eq!(key_value.endpoints.len(), 2);
        assert!(key_value.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::KeyValue
                && endpoint.protocol == LocalEndpointProtocol::Redis
                && endpoint.url.as_deref() == Some("redis://127.0.0.1:16379")
        }));
        assert!(key_value.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Guest
                && endpoint.purpose == LocalEndpointPurpose::KeyValue
                && endpoint.protocol == LocalEndpointProtocol::Redis
                && endpoint.url.as_deref() == Some("redis://host.internal:16379")
        }));
        let key_value_details = key_value.key_value.as_ref().unwrap();
        assert_eq!(key_value_details.database, 0);
        assert_eq!(key_value_details.implementation, "tinyredis");
        assert!(key_value_details.capabilities.ping);
        assert!(key_value_details.capabilities.strings);
        assert!(key_value_details.capabilities.targeted_lock_scripts);
        assert!(!key_value_details.capabilities.lua_scripts);

        let database = profile.services.get("database").unwrap();
        assert!(database.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::Database
                && endpoint.protocol == LocalEndpointProtocol::Postgres
        }));
    }

    #[tokio::test]
    async fn profile_route_publishes_active_sandbox_route_profiles() {
        let app = test_router();

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/sandboxes",
                Some(json!({
                    "routes": [{
                        "name": "preview",
                        "protocol": "http",
                        "target_port": 3000
                    }]
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let sandbox = json_body::<SandboxInfo>(response).await;
        assert_eq!(sandbox.routes.len(), 1);
        assert_eq!(sandbox.routes[0].url, "http://127.0.0.1:3000");

        let response = app
            .clone()
            .oneshot(request_with_host("GET", "/profile", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let profile = json_body::<LocalProfile>(response).await;
        assert_eq!(profile.routes.len(), 1);

        let route = &profile.routes[0];
        assert_eq!(route.name, "preview");
        assert_eq!(route.sandbox_id, Some(sandbox.id));
        assert_eq!(route.target_port, Some(3000));
        assert_eq!(route.health.status, LocalHealthStatus::Unknown);
        assert_eq!(route.endpoints.len(), 3);

        let endpoint = &route.endpoints[0];
        assert_eq!(endpoint.name, "host");
        assert_eq!(endpoint.visibility, LocalEndpointVisibility::Host);
        assert_eq!(endpoint.purpose, LocalEndpointPurpose::SandboxRoute);
        assert_eq!(endpoint.protocol, LocalEndpointProtocol::Http);
        assert_eq!(endpoint.url.as_deref(), Some("http://127.0.0.1:3000"));
        assert_eq!(endpoint.health.status, LocalHealthStatus::Unknown);
        let endpoint = &route.endpoints[1];
        assert_eq!(endpoint.name, "browser");
        assert_eq!(endpoint.visibility, LocalEndpointVisibility::Browser);
        assert_eq!(endpoint.purpose, LocalEndpointPurpose::SandboxRoute);
        assert_eq!(endpoint.protocol, LocalEndpointProtocol::Http);
        let browser_url = format!(
            "http://daemon.local:4567/proxy/routes/{}/preview",
            sandbox.id
        );
        assert_eq!(endpoint.url.as_deref(), Some(browser_url.as_str()));
        assert_eq!(endpoint.health.status, LocalHealthStatus::Unknown);
        let endpoint = &route.endpoints[2];
        assert_eq!(endpoint.name, "browser_websocket");
        assert_eq!(endpoint.visibility, LocalEndpointVisibility::Browser);
        assert_eq!(endpoint.purpose, LocalEndpointPurpose::Websocket);
        assert_eq!(endpoint.protocol, LocalEndpointProtocol::Ws);
        let websocket_url = format!("ws://daemon.local:4567/proxy/routes/{}/preview", sandbox.id);
        assert_eq!(endpoint.url.as_deref(), Some(websocket_url.as_str()));
        assert_eq!(endpoint.health.status, LocalHealthStatus::Unknown);
        assert!(route.endpoints.iter().all(|endpoint| {
            endpoint.visibility != LocalEndpointVisibility::Public
                && endpoint.protocol != LocalEndpointProtocol::Wss
                && endpoint.protocol != LocalEndpointProtocol::Https
        }));

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/sandboxes/{}", sandbox.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app.oneshot(request("GET", "/profile", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let profile = json_body::<LocalProfile>(response).await;
        assert!(profile.routes.is_empty());
    }

    #[tokio::test]
    async fn profile_routes_keep_duplicate_route_identity_by_sandbox_id() {
        let app = test_router();

        let first = create_sandbox_with_route(&app, "preview", "http", 3000).await;
        let second = create_sandbox_with_route(&app, "preview", "http", 3000).await;

        let profile = get_profile_with_host(&app).await;
        assert_eq!(profile.routes.len(), 2);
        for sandbox in [&first, &second] {
            let route = profile
                .routes
                .iter()
                .find(|route| route.sandbox_id == Some(sandbox.id))
                .unwrap();
            assert_eq!(route.name, "preview");
            assert_eq!(route.target_port, Some(3000));
            assert_eq!(
                route.endpoints[0].url.as_deref(),
                Some("http://127.0.0.1:3000")
            );
        }

        let response = app
            .clone()
            .oneshot(request("DELETE", &format!("/sandboxes/{}", first.id), None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let profile = get_profile_with_host(&app).await;
        assert_eq!(profile.routes.len(), 1);
        assert_eq!(profile.routes[0].sandbox_id, Some(second.id));
    }

    #[tokio::test]
    async fn create_sandbox_rejects_duplicate_route_names_in_one_sandbox() {
        let app = test_router();

        let response = app
            .oneshot(request(
                "POST",
                "/sandboxes",
                Some(json!({
                    "routes": [
                        { "name": "preview", "protocol": "http", "target_port": 3000 },
                        { "name": "preview", "protocol": "http", "target_port": 3001 }
                    ]
                })),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = json_body::<Value>(response).await;
        assert_eq!(body["code"], json!("invalid_request"));
        assert_eq!(
            body["message"],
            json!("invalid request: duplicate route name: preview")
        );
    }

    #[tokio::test]
    async fn profile_route_protocols_flow_through_without_future_endpoint_shapes() {
        let app = test_router();

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/sandboxes",
                Some(json!({
                    "routes": [
                        { "name": "web", "protocol": "http", "target_port": 3000 },
                        { "name": "secure", "protocol": "https", "target_port": 3443 },
                        { "name": "raw", "protocol": "tcp", "target_port": 4000 }
                    ]
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let sandbox = json_body::<SandboxInfo>(response).await;

        let profile = get_profile_with_host(&app).await;
        assert_eq!(profile.routes.len(), 3);
        assert!(profile
            .routes
            .iter()
            .all(|route| route.sandbox_id == Some(sandbox.id)));
        assert_route_endpoint(
            &profile,
            "web",
            LocalEndpointProtocol::Http,
            "http://127.0.0.1:3000",
        );
        let web = profile
            .routes
            .iter()
            .find(|route| route.name == "web")
            .unwrap();
        assert_eq!(web.endpoints.len(), 3);
        let expected_browser_url =
            format!("http://daemon.local:4567/proxy/routes/{}/web", sandbox.id);
        assert!(web.endpoints.iter().any(|endpoint| {
            endpoint.name == "browser"
                && endpoint.visibility == LocalEndpointVisibility::Browser
                && endpoint.purpose == LocalEndpointPurpose::SandboxRoute
                && endpoint.protocol == LocalEndpointProtocol::Http
                && endpoint.url.as_deref() == Some(expected_browser_url.as_str())
        }));
        let expected_websocket_url =
            format!("ws://daemon.local:4567/proxy/routes/{}/web", sandbox.id);
        assert!(web.endpoints.iter().any(|endpoint| {
            endpoint.name == "browser_websocket"
                && endpoint.visibility == LocalEndpointVisibility::Browser
                && endpoint.purpose == LocalEndpointPurpose::Websocket
                && endpoint.protocol == LocalEndpointProtocol::Ws
                && endpoint.url.as_deref() == Some(expected_websocket_url.as_str())
        }));
        assert_route_endpoint(
            &profile,
            "secure",
            LocalEndpointProtocol::Https,
            "https://127.0.0.1:3443",
        );
        assert_route_endpoint(
            &profile,
            "raw",
            LocalEndpointProtocol::Tcp,
            "tcp://127.0.0.1:4000",
        );
        let secure = profile
            .routes
            .iter()
            .find(|route| route.name == "secure")
            .unwrap();
        assert_eq!(secure.endpoints.len(), 1);
        let raw = profile
            .routes
            .iter()
            .find(|route| route.name == "raw")
            .unwrap();
        assert_eq!(raw.endpoints.len(), 1);
        assert!(profile
            .routes
            .iter()
            .flat_map(|route| &route.endpoints)
            .all(|endpoint| {
                endpoint.visibility != LocalEndpointVisibility::Public
                    && endpoint.protocol != LocalEndpointProtocol::Wss
                    && !(endpoint.visibility == LocalEndpointVisibility::Browser
                        && endpoint.protocol == LocalEndpointProtocol::Https)
            }));
    }

    #[tokio::test]
    async fn failed_delete_preserves_route_profile_facts() {
        let state = DaemonState::new(FailingDeleteProvider::new());
        let app = router_with_state(state);

        let sandbox = create_sandbox_with_route(&app, "preview", "http", 3000).await;
        let profile = get_profile(&app).await;
        assert_eq!(profile.routes.len(), 1);
        assert_eq!(profile.routes[0].sandbox_id, Some(sandbox.id));

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/sandboxes/{}", sandbox.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let profile = get_profile(&app).await;
        assert_eq!(profile.routes.len(), 1);
        assert_eq!(profile.routes[0].sandbox_id, Some(sandbox.id));
    }

    #[tokio::test]
    async fn proxy_route_forwards_http_requests_to_recorded_host_route() {
        let (upstream_port, upstream) = start_echo_upstream().await;
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "preview", "http", upstream_port).await;

        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/proxy/routes/{}/preview/nested/path?x=1&y=two", sandbox.id),
                Some(json!({ "body": true })),
            ))
            .await
            .unwrap();
        upstream.abort();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            json_body::<Value>(response).await,
            json!({
                "method": "PATCH",
                "path": "/nested/path",
                "query": "x=1&y=two",
                "body": "{\"body\":true}"
            })
        );
    }

    #[tokio::test]
    async fn proxy_route_preserves_encoded_tail_path() {
        let (upstream_port, upstream) = start_echo_upstream().await;
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "preview", "http", upstream_port).await;

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/proxy/routes/{}/preview/encoded/%2F/path?x=a%2Fb",
                    sandbox.id
                ),
                None,
            ))
            .await
            .unwrap();
        upstream.abort();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            json_body::<Value>(response).await,
            json!({
                "method": "GET",
                "path": "/encoded/%2F/path",
                "query": "x=a%2Fb",
                "body": ""
            })
        );
    }

    #[tokio::test]
    async fn proxy_route_returns_bad_gateway_for_unreachable_upstream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = listener.local_addr().unwrap().port();
        drop(listener);
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "preview", "http", upstream_port).await;

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/proxy/routes/{}/preview", sandbox.id),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("provider")
        );
    }

    #[tokio::test]
    async fn proxy_route_limits_upstream_response_body() {
        let (upstream_port, upstream) = start_large_body_upstream().await;
        let upstream_response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{upstream_port}"))
            .send()
            .await
            .unwrap();

        let error = read_limited_upstream_body_with_limit(upstream_response, 4)
            .await
            .unwrap_err();
        upstream.abort();

        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, SandboxErrorCode::Provider);
        assert_eq!(
            error.message,
            "upstream response exceeded proxy body limit of 4 bytes"
        );
    }

    #[tokio::test]
    async fn proxy_route_returns_not_found_after_sandbox_delete() {
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "preview", "http", 3000).await;

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/sandboxes/{}", sandbox.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/proxy/routes/{}/preview", sandbox.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("sandbox_not_found")
        );

        let profile = get_profile(&app).await;
        assert!(profile.routes.is_empty());
    }

    #[tokio::test]
    async fn proxy_route_returns_route_not_found_for_missing_route_name() {
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "preview", "http", 3000).await;

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/proxy/routes/{}/missing", sandbox.id),
                None,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("route_not_found")
        );
    }

    #[tokio::test]
    async fn proxy_route_rejects_non_http_routes_without_browser_tls_facts() {
        let app = test_router();
        let sandbox = create_sandbox_with_route(&app, "secure", "https", 3443).await;

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/proxy/routes/{}/secure", sandbox.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("unsupported")
        );

        let profile = get_profile_with_host(&app).await;
        let route = profile
            .routes
            .iter()
            .find(|route| route.name == "secure")
            .unwrap();
        assert_eq!(route.endpoints.len(), 1);
        assert!(route.endpoints.iter().all(|endpoint| {
            endpoint.visibility != LocalEndpointVisibility::Browser
                && endpoint.protocol != LocalEndpointProtocol::Wss
        }));
    }

    #[tokio::test]
    async fn profile_route_accepts_injected_service_profile_source() {
        #[derive(Clone)]
        struct TestProfileSource;

        impl ServiceProfileSource for TestProfileSource {
            fn services(&self) -> BTreeMap<String, LocalServiceProfile> {
                BTreeMap::from([(
                    "object_storage".to_string(),
                    LocalServiceProfile {
                        health: LocalHealth {
                            status: LocalHealthStatus::Ready,
                            message: None,
                            checked_at_unix_ms: None,
                        },
                        endpoints: vec![LocalEndpoint {
                            name: "host_api".to_string(),
                            visibility: LocalEndpointVisibility::Host,
                            purpose: LocalEndpointPurpose::ObjectStorageApi,
                            protocol: LocalEndpointProtocol::Http,
                            url: Some("http://127.0.0.1:19000".to_string()),
                            health: LocalHealth {
                                status: LocalHealthStatus::Ready,
                                message: None,
                                checked_at_unix_ms: None,
                            },
                        }],
                        credentials: None,
                        logs: Vec::new(),
                        object_storage: None,
                        key_value: None,
                    },
                )])
            }
        }

        let state =
            DaemonState::new(InMemorySandboxProvider::new()).with_profile_source(TestProfileSource);
        let app = router_with_state(state);

        let response = app.oneshot(request("GET", "/profile", None)).await.unwrap();

        let profile = json_body::<LocalProfile>(response).await;
        assert_eq!(
            profile.services["object_storage"].endpoints[0]
                .url
                .as_deref(),
            Some("http://127.0.0.1:19000")
        );
        assert!(!profile.services.contains_key("database"));
    }

    #[tokio::test]
    async fn profile_route_reflects_daemon_owned_object_storage_manager_state() {
        let config = ObjectStorageConfig {
            backend: ObjectStorageBackendKind::Minio,
            data_dir: std::env::temp_dir().join(format!(
                "local-sandbox-daemon-manager-fail-test-{}",
                std::process::id()
            )),
            minio_binary: Some(PathBuf::from("/fake/minio")),
            api_port: 19_000,
            readiness_timeout: Duration::from_millis(1),
            guest_endpoint_host: "guest.local".to_string(),
            ..ObjectStorageConfig::default()
        };
        let service = ObjectStorageService::with_components(
            config.clone(),
            Arc::new(FakeEmbeddedRunner),
            Arc::new(FakeResolver),
            Arc::new(FakeRunner),
            Arc::new(FailingProbe),
        );
        let object_storage = ObjectStorageManager::new(service);
        let state = DaemonState::with_object_storage(
            InMemorySandboxProvider::new(),
            object_storage.clone(),
        );
        let app = router_with_state(state);

        let result = object_storage.start().await;
        assert!(result.is_err());

        let response = app.oneshot(request("GET", "/profile", None)).await.unwrap();

        let profile = json_body::<LocalProfile>(response).await;
        let object_storage_profile = profile.services.get("object_storage").unwrap();
        assert_eq!(
            object_storage_profile.health.status,
            LocalHealthStatus::Failed
        );
        assert_eq!(
            object_storage_profile.health.message.as_deref(),
            Some("provider error: minio unavailable")
        );
        assert!(object_storage_profile.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.url.as_deref() == Some("http://127.0.0.1:19000")
        }));
        assert!(object_storage_profile.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Guest
                && endpoint.url.as_deref() == Some("http://guest.local:19000")
        }));
        let _ = std::fs::remove_dir_all(config.data_dir);
    }

    #[tokio::test]
    async fn sandbox_command_file_timeout_and_snapshot_routes_work() {
        let app = test_router();

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/sandboxes",
                Some(json!({ "architecture": "arm64", "timeout_ms": 1000 })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let sandbox = json_body::<SandboxInfo>(response).await;
        assert_eq!(sandbox.status, SandboxStatus::Running);

        let response = app
            .clone()
            .oneshot(request("GET", &format!("/sandboxes/{}", sandbox.id), None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body::<SandboxInfo>(response).await.id, sandbox.id);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/sandboxes/{}/commands", sandbox.id),
                Some(json!({ "argv": ["echo", "hello"] })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let command = json_body::<CommandInfo>(response).await;

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/sandboxes/{}/commands/{}?wait=true&timeout_ms=25",
                    sandbox.id, command.id
                ),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body::<CommandInfo>(response).await.id, command.id);

        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/sandboxes/{}/commands/{}", sandbox.id, command.id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/sandboxes/{}/files", sandbox.id),
                Some(json!({
                    "files": [{
                        "path": "/workspace/index.ts",
                        "contents_base64": "Y29uc29sZS5sb2coJ2hpJyk=",
                        "mode": 420
                    }]
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body::<WriteFilesResponse>(response).await.paths,
            vec!["/workspace/index.ts"]
        );

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/sandboxes/{}/timeout", sandbox.id),
                Some(json!({ "additional_ms": 1000 })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(request(
                "POST",
                &format!("/sandboxes/{}/snapshots", sandbox.id),
                Some(json!({ "label": "checkpoint" })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let snapshot = json_body::<SnapshotInfo>(response).await;
        assert_eq!(snapshot.sandbox_id, sandbox.id);
        assert_eq!(snapshot.label.as_deref(), Some("checkpoint"));
    }

    #[tokio::test]
    async fn daemon_maps_provider_errors_to_http_status_codes() {
        let app = test_router();
        let missing_sandbox = SandboxId::new();

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/sandboxes/{missing_sandbox}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("sandbox_not_found")
        );

        let response = app
            .clone()
            .oneshot(request("POST", "/sandboxes", Some(json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let sandbox = json_body::<SandboxInfo>(response).await;

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/sandboxes/{}/commands", sandbox.id),
                Some(json!({ "argv": [] })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .oneshot(request(
                "PUT",
                &format!("/sandboxes/{}/network-policy", sandbox.id),
                Some(json!({ "mode": "deny_all" })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            json_body::<Value>(response).await["code"],
            json!("unsupported")
        );
    }

    fn test_router() -> Router {
        router_with_state(DaemonState::new(InMemorySandboxProvider::new()))
    }

    async fn create_sandbox_with_route(
        app: &Router,
        name: &str,
        protocol: &str,
        target_port: u16,
    ) -> SandboxInfo {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/sandboxes",
                Some(json!({
                    "routes": [{
                        "name": name,
                        "protocol": protocol,
                        "target_port": target_port
                    }]
                })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body::<SandboxInfo>(response).await
    }

    async fn get_profile(app: &Router) -> LocalProfile {
        let response = app
            .clone()
            .oneshot(request("GET", "/profile", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body::<LocalProfile>(response).await
    }

    async fn get_profile_with_host(app: &Router) -> LocalProfile {
        let response = app
            .clone()
            .oneshot(request_with_host("GET", "/profile", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body::<LocalProfile>(response).await
    }

    fn assert_route_endpoint(
        profile: &LocalProfile,
        name: &str,
        protocol: LocalEndpointProtocol,
        url: &str,
    ) {
        let route = profile
            .routes
            .iter()
            .find(|route| route.name == name)
            .unwrap();
        assert_eq!(route.health.status, LocalHealthStatus::Unknown);
        let endpoint = route
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == "host")
            .unwrap();
        assert_eq!(endpoint.name, "host");
        assert_eq!(endpoint.visibility, LocalEndpointVisibility::Host);
        assert_eq!(endpoint.purpose, LocalEndpointPurpose::SandboxRoute);
        assert_eq!(endpoint.protocol, protocol);
        assert_eq!(endpoint.url.as_deref(), Some(url));
        assert_eq!(endpoint.health.status, LocalHealthStatus::Unknown);
    }

    fn request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        builder
            .body(match body {
                Some(value) => Body::from(value.to_string()),
                None => Body::empty(),
            })
            .unwrap()
    }

    fn request_with_host(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
        let mut request = request(method, uri, body);
        request
            .headers_mut()
            .insert(header::HOST, "daemon.local:4567".parse().unwrap());
        request
    }

    async fn start_echo_upstream() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(echo_upstream);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (port, handle)
    }

    async fn start_large_body_upstream() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(large_body_upstream);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (port, handle)
    }

    async fn echo_upstream(method: Method, uri: Uri, body: Body) -> impl IntoResponse {
        let body = axum::body::to_bytes(body, RESPONSE_BODY_LIMIT)
            .await
            .unwrap();
        (
            StatusCode::ACCEPTED,
            Json(json!({
                "method": method.as_str(),
                "path": uri.path(),
                "query": uri.query().unwrap_or(""),
                "body": String::from_utf8(body.to_vec()).unwrap(),
            })),
        )
    }

    async fn large_body_upstream() -> impl IntoResponse {
        (StatusCode::OK, "0123456789")
    }

    async fn json_body<T: DeserializeOwned>(response: Response) -> T {
        let bytes = axum::body::to_bytes(response.into_body(), RESPONSE_BODY_LIMIT)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[derive(Clone)]
    struct FailingDeleteProvider {
        inner: InMemorySandboxProvider,
    }

    impl FailingDeleteProvider {
        fn new() -> Self {
            Self {
                inner: InMemorySandboxProvider::new(),
            }
        }
    }

    impl SandboxProvider for FailingDeleteProvider {
        fn capabilities(&self) -> ProviderFuture<'_, SandboxCapabilities> {
            self.inner.capabilities()
        }

        fn create_sandbox(&self, request: CreateSandboxRequest) -> ProviderFuture<'_, SandboxInfo> {
            self.inner.create_sandbox(request)
        }

        fn get_sandbox(&self, sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
            self.inner.get_sandbox(sandbox_id)
        }

        fn delete_sandbox(&self, _sandbox_id: SandboxId) -> ProviderFuture<'_, SandboxInfo> {
            Box::pin(async { Err(SandboxError::provider("delete failed")) })
        }

        fn create_command(
            &self,
            sandbox_id: SandboxId,
            request: CreateCommandRequest,
        ) -> ProviderFuture<'_, CommandInfo> {
            self.inner.create_command(sandbox_id, request)
        }

        fn get_command(
            &self,
            sandbox_id: SandboxId,
            command_id: CommandId,
            wait: CommandWait,
        ) -> ProviderFuture<'_, CommandInfo> {
            self.inner.get_command(sandbox_id, command_id, wait)
        }

        fn cancel_command(
            &self,
            sandbox_id: SandboxId,
            command_id: CommandId,
        ) -> ProviderFuture<'_, CommandInfo> {
            self.inner.cancel_command(sandbox_id, command_id)
        }

        fn write_files(
            &self,
            sandbox_id: SandboxId,
            request: WriteFilesRequest,
        ) -> ProviderFuture<'_, WriteFilesResponse> {
            self.inner.write_files(sandbox_id, request)
        }

        fn extend_timeout(
            &self,
            sandbox_id: SandboxId,
            request: ExtendSandboxTimeoutRequest,
        ) -> ProviderFuture<'_, local_sandbox_core::SandboxTimeoutInfo> {
            self.inner.extend_timeout(sandbox_id, request)
        }

        fn set_network_policy(
            &self,
            sandbox_id: SandboxId,
            request: SetNetworkPolicyRequest,
        ) -> ProviderFuture<'_, ()> {
            self.inner.set_network_policy(sandbox_id, request)
        }

        fn create_snapshot(
            &self,
            sandbox_id: SandboxId,
            request: CreateSnapshotRequest,
        ) -> ProviderFuture<'_, SnapshotInfo> {
            self.inner.create_snapshot(sandbox_id, request)
        }
    }

    struct FakeResolver;

    struct FakeEmbeddedRunner;

    impl EmbeddedObjectStorageRunner for FakeEmbeddedRunner {
        fn start(
            &self,
            _config: &ObjectStorageConfig,
        ) -> SandboxResult<EmbeddedObjectStorageHandle> {
            Ok(EmbeddedObjectStorageHandle {
                id: "fake-embedded-s3".to_string(),
            })
        }

        fn stop(&self, _handle: &EmbeddedObjectStorageHandle) -> SandboxResult<()> {
            Ok(())
        }
    }

    impl MinioBinaryResolver for FakeResolver {
        fn resolve(&self, config: &ObjectStorageConfig) -> SandboxResult<PathBuf> {
            Ok(config.minio_binary.clone().unwrap())
        }
    }

    struct FakeRunner;

    impl MinioProcessRunner for FakeRunner {
        fn start(
            &self,
            _config: &ObjectStorageConfig,
            binary: &Path,
        ) -> SandboxResult<MinioProcessHandle> {
            assert_eq!(binary, Path::new("/fake/minio"));
            Ok(MinioProcessHandle {
                id: "fake-minio".to_string(),
            })
        }

        fn stop(&self, _handle: &MinioProcessHandle) -> SandboxResult<()> {
            Ok(())
        }
    }

    struct FailingProbe;

    impl ObjectStorageHealthProbe for FailingProbe {
        fn wait_until_ready<'a>(
            &'a self,
            _config: &'a ObjectStorageConfig,
        ) -> Pin<Box<dyn Future<Output = SandboxResult<()>> + Send + 'a>> {
            Box::pin(async { Err(SandboxError::provider("minio unavailable")) })
        }
    }
}
