//! Disk lifecycle seam for vfkit sandboxes.

use crate::VfkitConfig;
use local_sandbox_core::{SandboxError, SandboxId, SandboxResult};
use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Disk state allocated for a sandbox runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiskHandle {
    pub(crate) sandbox_id: SandboxId,
    pub(crate) path: SandboxDiskPath,
}

/// Validated sandbox disk directory path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SandboxDiskPath(PathBuf);

impl SandboxDiskPath {
    pub(crate) fn new(root_dir: &Path, sandbox_dir_name: &str) -> SandboxResult<Self> {
        let path = root_dir.join(sandbox_dir_name);
        if !path.starts_with(root_dir) {
            return Err(SandboxError::invalid_request(
                "sandbox disk path must stay inside root_dir",
            ));
        }
        if !path.is_absolute() {
            return Err(SandboxError::invalid_request(
                "sandbox disk path must be absolute",
            ));
        }

        #[cfg(unix)]
        if path.as_os_str().as_bytes().contains(&0) {
            return Err(SandboxError::invalid_request(
                "sandbox disk path must not contain null bytes",
            ));
        }

        Ok(Self(path))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Fakeable disk lifecycle boundary.
pub(crate) trait DiskManager: Send + Sync {
    fn prepare(&self, sandbox_id: SandboxId) -> SandboxResult<DiskHandle>;

    fn cleanup(&self, handle: &DiskHandle) -> SandboxResult<()>;
}

/// Filesystem-backed disk manager used by the default provider.
pub(crate) struct FilesystemDiskManager {
    root_dir: PathBuf,
}

impl FilesystemDiskManager {
    #[must_use]
    pub(crate) fn new(config: &VfkitConfig) -> Self {
        Self {
            root_dir: trusted_root_dir(&config.root_dir),
        }
    }
}

impl DiskManager for FilesystemDiskManager {
    fn prepare(&self, sandbox_id: SandboxId) -> SandboxResult<DiskHandle> {
        let root_dir = self.root_dir.clone();
        let sandbox_dir_name = sandbox_id.to_string();
        if !sandbox_dir_name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(SandboxError::invalid_request("invalid sandbox id"));
        }

        let path = SandboxDiskPath::new(&root_dir, &sandbox_dir_name)?;
        create_sandbox_dir(&path)?;
        Ok(DiskHandle { sandbox_id, path })
    }

    fn cleanup(&self, handle: &DiskHandle) -> SandboxResult<()> {
        cleanup_sandbox_dir(&handle.path)
    }
}

fn create_sandbox_dir(path: &SandboxDiskPath) -> SandboxResult<()> {
    fs::create_dir_all(path.as_path())
        .map_err(|error| SandboxError::provider(format!("failed to prepare sandbox disk: {error}")))
}

fn cleanup_sandbox_dir(path: &SandboxDiskPath) -> SandboxResult<()> {
    match fs::remove_dir_all(path.as_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SandboxError::provider(format!(
            "failed to clean sandbox disk: {error}"
        ))),
    }
}

fn trusted_root_dir(configured_root_dir: &Path) -> PathBuf {
    if configured_root_dir.is_absolute() && !configured_root_dir.as_os_str().is_empty() {
        return configured_root_dir.to_path_buf();
    }

    env::temp_dir().join("local-sandbox-vfkit")
}
