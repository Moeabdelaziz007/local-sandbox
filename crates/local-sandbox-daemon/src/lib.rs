//! HTTP daemon for local sandbox clients.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use local_sandbox_core::{
    CommandId, CommandWait, CreateCommandRequest, CreateSandboxRequest, CreateSnapshotRequest,
    ExtendSandboxTimeoutRequest, InMemorySandboxProvider, LocalCredentials, LocalDaemonProfile,
    LocalEndpoint, LocalEndpointProtocol, LocalEndpointPurpose, LocalEndpointVisibility,
    LocalHealth, LocalHealthStatus, LocalProfile, LocalSandboxProfile, LocalServiceProfile,
    SandboxError, SandboxErrorCode, SandboxId, SandboxProvider, SetNetworkPolicyRequest,
    WriteFilesRequest,
};
use local_sandbox_services::{object_storage_profile, ObjectStorageConfig};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Shared daemon application state.
#[derive(Clone)]
pub struct DaemonState {
    provider: Arc<dyn SandboxProvider>,
    profile_source: Arc<dyn ServiceProfileSource>,
}

impl DaemonState {
    /// Create daemon state from a sandbox provider.
    #[must_use]
    pub fn new(provider: impl SandboxProvider) -> Self {
        Self {
            provider: Arc::new(provider),
            profile_source: Arc::new(DefaultServiceProfileSource::default()),
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

    async fn profile(&self) -> Result<LocalProfile, SandboxError> {
        let capabilities = self.provider.capabilities().await?;
        Ok(default_profile(
            capabilities.provider,
            self.profile_source.services(),
        ))
    }
}

/// Source of product-neutral local service profile facts.
pub trait ServiceProfileSource: Send + Sync {
    /// Return local service profiles keyed by product-neutral service name.
    fn services(&self) -> BTreeMap<String, LocalServiceProfile>;
}

#[derive(Default)]
struct DefaultServiceProfileSource {
    object_storage: ObjectStorageConfig,
}

impl ServiceProfileSource for DefaultServiceProfileSource {
    fn services(&self) -> BTreeMap<String, LocalServiceProfile> {
        let mut services = default_unknown_services();
        services.insert(
            "object_storage".to_string(),
            object_storage_profile(&self.object_storage),
        );
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

async fn profile(State(state): State<DaemonState>) -> Result<impl IntoResponse, DaemonError> {
    Ok(Json(state.profile().await?))
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
        Json(state.provider.create_sandbox(request).await?),
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
    Ok(Json(state.provider.delete_sandbox(sandbox_id).await?))
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
            SandboxErrorCode::SandboxNotFound | SandboxErrorCode::CommandNotFound => {
                StatusCode::NOT_FOUND
            }
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

fn default_profile(
    provider: String,
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
        routes: Vec::new(),
        services,
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
    use axum::{body::Body, http::Request};
    use local_sandbox_core::{
        CommandInfo, SandboxCapabilities, SandboxInfo, SandboxStatus, SnapshotInfo,
        WriteFilesResponse,
    };
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};
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

        let database = profile.services.get("database").unwrap();
        assert!(database.endpoints.iter().any(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::Database
                && endpoint.protocol == LocalEndpointProtocol::Postgres
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

    async fn json_body<T: DeserializeOwned>(response: Response) -> T {
        let bytes = axum::body::to_bytes(response.into_body(), RESPONSE_BODY_LIMIT)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
