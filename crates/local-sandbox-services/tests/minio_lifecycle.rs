//! Ignored host contract tests for a real `MinIO` binary.

use local_sandbox_core::LocalHealthStatus;
use local_sandbox_services::{ObjectStorageConfig, ObjectStorageManager, ENV_MINIO_BINARY};
use std::{path::PathBuf, time::Duration};

#[tokio::test]
#[ignore = "requires a real minio binary and free object-storage ports"]
async fn starts_real_minio_and_serves_host_profile() {
    let temp_root = std::env::temp_dir().join(format!(
        "local-sandbox-real-minio-test-{}",
        std::process::id()
    ));
    let log_path = temp_root.join("minio.log");
    let config = ObjectStorageConfig {
        data_dir: temp_root.join("data"),
        minio_binary: std::env::var_os(ENV_MINIO_BINARY).map(PathBuf::from),
        api_port: 19_000,
        console_port: 19_001,
        log_path: Some(log_path),
        readiness_timeout: Duration::from_secs(10),
        ..ObjectStorageConfig::default()
    };
    let manager = ObjectStorageManager::from_config(config);

    manager.start().await.unwrap();

    let profile = manager.profile();
    assert_eq!(profile.health.status, LocalHealthStatus::Ready);
    assert!(profile.endpoints.iter().any(|endpoint| {
        endpoint.name == "host_api" && endpoint.url.as_deref() == Some("http://127.0.0.1:19000")
    }));

    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(temp_root);
}
