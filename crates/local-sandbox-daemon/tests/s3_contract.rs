//! Daemon S3 compatibility contracts.

use aws_credential_types::Credentials;
use aws_sdk_s3::{
    config::{BehaviorVersion, Region},
    presigning::{PresignedRequest, PresigningConfig},
    primitives::ByteStream,
    types::{Delete, ObjectIdentifier},
    Client, Config,
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use local_sandbox_core::{
    LocalEndpointProtocol, LocalEndpointPurpose, LocalEndpointVisibility, LocalHealthStatus,
    LocalObjectStorageCapabilities, LocalProfile, LocalServiceProfile,
};
use local_sandbox_daemon::{boot_state_with_object_storage_config, router_with_state, DaemonState};
use local_sandbox_services::{ObjectStorageBackendKind, ObjectStorageConfig, ENV_MINIO_BINARY};
use std::{
    env,
    error::Error,
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

const RUN_REAL_MINIO_TESTS_ENV: &str = "LOCAL_SANDBOX_RUN_MINIO_S3_CONTRACT";
const RESPONSE_BODY_LIMIT: usize = 1024 * 1024;

#[tokio::test]
async fn daemon_profile_drives_embedded_s3_host_contract() -> Result<(), Box<dyn Error>> {
    prove_daemon_profile_drives_s3_host_contract(ObjectStorageBackendKind::Embedded).await
}

#[tokio::test]
#[ignore = "requires LOCAL_SANDBOX_RUN_MINIO_S3_CONTRACT=1 and a real minio binary"]
async fn daemon_profile_drives_real_minio_s3_host_contract() -> Result<(), Box<dyn Error>> {
    prove_daemon_profile_drives_s3_host_contract(ObjectStorageBackendKind::Minio).await
}

async fn prove_daemon_profile_drives_s3_host_contract(
    backend: ObjectStorageBackendKind,
) -> Result<(), Box<dyn Error>> {
    if backend == ObjectStorageBackendKind::Minio && !real_minio_tests_enabled() {
        eprintln!("skipping real MinIO S3 contract; set {RUN_REAL_MINIO_TESTS_ENV}=1 to run");
        return Ok(());
    }
    let temp_root = unique_temp_root();
    let state = boot_state_for_contract(temp_root.as_path(), backend).await?;
    let _cleanup = DaemonCleanup::new(state.clone(), temp_root);
    let profile = fetch_profile(state).await?;
    let service = object_storage_service(&profile)?;

    assert_eq!(service.health.status, LocalHealthStatus::Ready);
    assert_profile_capabilities(service)?;

    let client = s3_client_from_profile(service)?;
    prove_host_s3_contract(&client).await?;

    Ok(())
}

async fn boot_state_for_contract(
    temp_root: &Path,
    backend: ObjectStorageBackendKind,
) -> Result<DaemonState, Box<dyn Error>> {
    let mut last_error = None;
    for _ in 0..10 {
        let config = contract_config(temp_root, backend)?;
        match boot_state_with_object_storage_config(config).await {
            Ok(state)
                if state.object_storage_manager().profile().health.status
                    == LocalHealthStatus::Ready =>
            {
                return Ok(state);
            }
            Ok(state) => {
                let profile = state.object_storage_manager().profile();
                let message = profile
                    .health
                    .message
                    .unwrap_or_else(|| "object storage did not become ready".to_string());
                let _ = state.object_storage_manager().shutdown();
                if !is_address_in_use_message(&message) {
                    return Err(invalid_data(message).into());
                }
                last_error = Some(message);
            }
            Err(error) if is_address_in_use_message(&error.to_string()) => {
                last_error = Some(error.to_string());
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(invalid_data(format!(
        "object-storage startup did not get free ports after retries: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
    .into())
}

fn contract_config(
    temp_root: &Path,
    backend: ObjectStorageBackendKind,
) -> Result<ObjectStorageConfig, io::Error> {
    let (api_port, console_port) = free_ports()?;
    Ok(ObjectStorageConfig {
        backend,
        data_dir: temp_root.join("data"),
        minio_binary: env::var_os(ENV_MINIO_BINARY).map(PathBuf::from),
        api_host: "127.0.0.1".to_string(),
        api_port,
        console_host: "127.0.0.1".to_string(),
        console_port,
        host_endpoint_host: "127.0.0.1".to_string(),
        guest_endpoint_host: "host.internal".to_string(),
        region: "us-east-1".to_string(),
        access_key_id: "local-sandbox-contract".to_string(),
        secret_access_key: "local-sandbox-contract-secret".to_string(),
        log_path: Some(temp_root.join("object-storage.log")),
        readiness_timeout: Duration::from_secs(30),
    })
}

fn is_address_in_use_message(message: &str) -> bool {
    message.contains("Address already in use") || message.contains("address already in use")
}

async fn prove_host_s3_contract(client: &Client) -> Result<(), Box<dyn Error>> {
    let bucket = format!("local-sandbox-contract-{}", unique_suffix());
    let source_key = "contract/source.txt";
    let copied_key = "contract/copied.txt";
    let presigned_put_key = "contract/presigned-put.txt";
    let source_payload = b"profile-derived aws sdk payload";
    let presigned_put_payload = b"profile-derived presigned put payload";

    client.create_bucket().bucket(&bucket).send().await?;

    client
        .put_object()
        .bucket(&bucket)
        .key(source_key)
        .body(ByteStream::from_static(source_payload))
        .send()
        .await?;

    let source_body = read_object(client, &bucket, source_key).await?;
    assert_eq!(source_body, source_payload);

    let source_head = client
        .head_object()
        .bucket(&bucket)
        .key(source_key)
        .send()
        .await?;
    assert_eq!(
        source_head.content_length(),
        Some(i64::try_from(source_payload.len())?)
    );

    let list = client
        .list_objects_v2()
        .bucket(&bucket)
        .prefix("contract/")
        .send()
        .await?;
    assert!(list
        .contents()
        .iter()
        .any(|object| object.key() == Some(source_key)));

    client
        .copy_object()
        .bucket(&bucket)
        .key(copied_key)
        .copy_source(format!("{bucket}/{source_key}"))
        .send()
        .await?;
    let copied_body = read_object(client, &bucket, copied_key).await?;
    assert_eq!(copied_body, source_payload);

    prove_presigned_contract(
        client,
        &bucket,
        source_key,
        presigned_put_key,
        source_payload,
        presigned_put_payload,
    )
    .await?;

    client
        .delete_object()
        .bucket(&bucket)
        .key(source_key)
        .send()
        .await?;

    let delete = Delete::builder()
        .objects(ObjectIdentifier::builder().key(copied_key).build()?)
        .objects(ObjectIdentifier::builder().key(presigned_put_key).build()?)
        .build()?;
    let delete_result = client
        .delete_objects()
        .bucket(&bucket)
        .delete(delete)
        .send()
        .await?;
    assert!(delete_result
        .deleted()
        .iter()
        .any(|deleted| deleted.key() == Some(copied_key)));
    assert!(delete_result
        .deleted()
        .iter()
        .any(|deleted| deleted.key() == Some(presigned_put_key)));

    client.delete_bucket().bucket(&bucket).send().await?;

    Ok(())
}

async fn prove_presigned_contract(
    client: &Client,
    bucket: &str,
    source_key: &str,
    presigned_put_key: &str,
    source_payload: &[u8],
    presigned_put_payload: &[u8],
) -> Result<(), Box<dyn Error>> {
    let http_client = presigned_http_client()?;
    let presigned_get = client
        .get_object()
        .bucket(bucket)
        .key(source_key)
        .presigned(presigning_config()?)
        .await?;
    assert_eq!(presigned_get.method(), "GET");
    let presigned_get_response =
        with_presigned_headers(http_client.get(presigned_get.uri()), &presigned_get)
            .send()
            .await?
            .error_for_status()?;
    let presigned_get_body = presigned_get_response.bytes().await?;
    assert_eq!(presigned_get_body.as_ref(), source_payload);

    let presigned_put = client
        .put_object()
        .bucket(bucket)
        .key(presigned_put_key)
        .body(ByteStream::from(presigned_put_payload.to_vec()))
        .presigned(presigning_config()?)
        .await?;
    assert_eq!(presigned_put.method(), "PUT");
    with_presigned_headers(http_client.put(presigned_put.uri()), &presigned_put)
        .body(presigned_put_payload.to_vec())
        .send()
        .await?
        .error_for_status()?;
    let presigned_put_body = read_object(client, bucket, presigned_put_key).await?;
    assert_eq!(presigned_put_body, presigned_put_payload);

    Ok(())
}

fn presigned_http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
}

async fn read_object(client: &Client, bucket: &str, key: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = client.get_object().bucket(bucket).key(key).send().await?;
    Ok(output.body.collect().await?.into_bytes().to_vec())
}

fn s3_client_from_profile(service: &LocalServiceProfile) -> Result<Client, Box<dyn Error>> {
    let object_storage = service
        .object_storage
        .as_ref()
        .ok_or_else(|| invalid_data("object_storage profile facts are missing"))?;
    let credentials = service
        .credentials
        .as_ref()
        .ok_or_else(|| invalid_data("object_storage credentials are missing"))?;
    let access_key_id = credentials
        .access_key_id
        .clone()
        .ok_or_else(|| invalid_data("object_storage access_key_id is missing"))?;
    let secret_access_key = credentials
        .secret_access_key
        .clone()
        .ok_or_else(|| invalid_data("object_storage secret_access_key is missing"))?;

    let config = Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(object_storage.region.clone()))
        .endpoint_url(host_api_endpoint(service)?)
        .force_path_style(object_storage.force_path_style)
        .credentials_provider(Credentials::new(
            access_key_id,
            secret_access_key,
            None,
            None,
            "local-sandbox-profile",
        ))
        .build();

    Ok(Client::from_conf(config))
}

fn object_storage_service(profile: &LocalProfile) -> Result<&LocalServiceProfile, io::Error> {
    profile
        .services
        .get("object_storage")
        .ok_or_else(|| invalid_data("profile missing object_storage service"))
}

fn host_api_endpoint(service: &LocalServiceProfile) -> Result<String, io::Error> {
    service
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::ObjectStorageApi
                && endpoint.protocol == LocalEndpointProtocol::Http
        })
        .and_then(|endpoint| endpoint.url.clone())
        .ok_or_else(|| invalid_data("object_storage host API endpoint is missing"))
}

fn assert_profile_capabilities(service: &LocalServiceProfile) -> Result<(), io::Error> {
    let object_storage = service
        .object_storage
        .as_ref()
        .ok_or_else(|| invalid_data("object_storage profile facts are missing"))?;
    let capabilities = object_storage.capabilities;

    assert_supported_capabilities(capabilities);
    assert!(!capabilities.cors);
    assert!(!capabilities.browser_upload);
    assert!(!capabilities.versioning);
    assert!(!capabilities.list_object_versions);

    Ok(())
}

fn assert_supported_capabilities(capabilities: LocalObjectStorageCapabilities) {
    assert!(capabilities.create_bucket);
    assert!(capabilities.put_object);
    assert!(capabilities.get_object);
    assert!(capabilities.head_object);
    assert!(capabilities.delete_object);
    assert!(capabilities.delete_objects);
    assert!(capabilities.list_objects_v2);
    assert!(capabilities.copy_object);
    assert!(capabilities.presigned_get);
    assert!(capabilities.presigned_put);
}

async fn fetch_profile(state: DaemonState) -> Result<LocalProfile, Box<dyn Error>> {
    let response = router_with_state(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/profile")
                .body(Body::empty())?,
        )
        .await?;
    if response.status() != StatusCode::OK {
        return Err(invalid_data(format!("profile route returned {}", response.status())).into());
    }

    let bytes = to_bytes(response.into_body(), RESPONSE_BODY_LIMIT).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn with_presigned_headers(
    mut builder: reqwest::RequestBuilder,
    request: &PresignedRequest,
) -> reqwest::RequestBuilder {
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }
    builder
}

fn presigning_config() -> Result<PresigningConfig, Box<dyn Error>> {
    Ok(PresigningConfig::expires_in(Duration::from_secs(60))?)
}

fn real_minio_tests_enabled() -> bool {
    matches!(
        env::var(RUN_REAL_MINIO_TESTS_ENV).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn free_ports() -> io::Result<(u16, u16)> {
    let api = TcpListener::bind(("127.0.0.1", 0))?;
    let console = TcpListener::bind(("127.0.0.1", 0))?;
    Ok((api.local_addr()?.port(), console.local_addr()?.port()))
}

fn unique_temp_root() -> PathBuf {
    env::temp_dir().join(format!("local-sandbox-s3-contract-{}", unique_suffix()))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

struct DaemonCleanup {
    state: DaemonState,
    temp_root: PathBuf,
}

impl DaemonCleanup {
    fn new(state: DaemonState, temp_root: PathBuf) -> Self {
        Self { state, temp_root }
    }
}

impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        let _ = self.state.object_storage_manager().shutdown();
        let _ = remove_dir_all_if_exists(&self.temp_root);
    }
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
