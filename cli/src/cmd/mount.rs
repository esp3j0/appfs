#[cfg(any(unix, target_os = "windows"))]
use agentfs_sdk::FileSystem;
use agentfs_sdk::{error::Error as SdkError, AgentFSOptions};
#[cfg(unix)]
use anyhow::Context;
#[cfg(target_os = "windows")]
use anyhow::Context;
use anyhow::Result;
use std::{collections::HashSet, path::PathBuf, sync::Arc};

#[cfg(unix)]
use agentfs_sdk::{HostFS, OverlayFS};
#[cfg(target_os = "windows")]
use std::path::Path;
#[cfg(unix)]
use std::{path::Path, process::Command};
#[cfg(any(unix, target_os = "windows"))]
use tokio::sync::Mutex;
#[cfg(unix)]
use turso::value::Value;

#[cfg(unix)]
use crate::mount::mount_fs;
use crate::mount::MountOpts;
#[cfg(unix)]
use crate::nfs::AgentNFS;
#[cfg(unix)]
use crate::nfsserve::tcp::NFSTcp;

#[cfg(target_os = "linux")]
use agentfs_sdk::{get_mounts, Mount};
#[cfg(target_os = "linux")]
use std::{
    io::{self, ErrorKind, Write},
    os::unix::fs::MetadataExt,
};

#[cfg(any(unix, target_os = "windows"))]
use crate::cmd::appfs::{self, AppfsBridgeCliArgs, AppfsRuntimeCliArgs};
#[cfg(any(unix, target_os = "windows"))]
use crate::cmd::init::open_agentfs;
#[cfg(target_os = "linux")]
use crate::fuse::FuseMountOptions;

pub use crate::opts::MountBackend;

/// Default NFS port to try (use a high port to avoid needing root)
#[cfg(unix)]
const DEFAULT_NFS_PORT: u32 = 11111;

/// Arguments for the mount command.
#[derive(Debug, Clone)]
pub struct MountArgs {
    /// The agent filesystem ID or path.
    pub id_or_path: String,
    /// The mountpoint path.
    pub mountpoint: PathBuf,
    /// Automatically unmount when the process exits.
    pub auto_unmount: bool,
    /// Allow root to access the mount.
    pub allow_root: bool,
    /// Allow other system users to access the mount.
    pub allow_other: bool,
    /// Run in foreground (don't daemonize).
    pub foreground: bool,
    /// User ID to report for all files (defaults to current user).
    pub uid: Option<u32>,
    /// Group ID to report for all files (defaults to current group).
    pub gid: Option<u32>,
    /// The mount backend to use (fuse or nfs).
    pub backend: MountBackend,
    /// Enable AppFS snapshot read-through for the given app ID.
    pub appfs_app_id: Option<String>,
    /// Additional AppFS app IDs for mount-side read-through.
    pub appfs_app_ids: Vec<String>,
    /// Session ID used for mount-side AppFS connector calls.
    pub appfs_session: Option<String>,
    /// Optional HTTP bridge endpoint for mount-side AppFS read-through.
    pub adapter_http_endpoint: Option<String>,
    /// HTTP bridge request timeout in milliseconds.
    pub adapter_http_timeout_ms: u64,
    /// Optional gRPC bridge endpoint for mount-side AppFS read-through.
    pub adapter_grpc_endpoint: Option<String>,
    /// gRPC bridge request timeout in milliseconds.
    pub adapter_grpc_timeout_ms: u64,
    /// Max retry count for bridge transport failures.
    pub adapter_bridge_max_retries: u32,
    /// Initial backoff in milliseconds for bridge retries.
    pub adapter_bridge_initial_backoff_ms: u64,
    /// Max backoff in milliseconds for bridge retries.
    pub adapter_bridge_max_backoff_ms: u64,
    /// Consecutive bridge transport failures required to open circuit breaker.
    pub adapter_bridge_circuit_breaker_failures: u32,
    /// Circuit breaker cooldown in milliseconds before retrying bridge calls.
    pub adapter_bridge_circuit_breaker_cooldown_ms: u64,
}

fn has_appfs_mount_config(args: &MountArgs) -> bool {
    args.appfs_app_id.is_some()
        || !args.appfs_app_ids.is_empty()
        || args.appfs_session.is_some()
        || args.adapter_http_endpoint.is_some()
        || args.adapter_grpc_endpoint.is_some()
}

#[cfg(any(unix, target_os = "windows"))]
fn appfs_mount_runtime_args(args: &MountArgs) -> Result<Vec<AppfsRuntimeCliArgs>> {
    appfs::build_runtime_cli_args(
        args.appfs_app_id.clone(),
        args.appfs_app_ids.clone(),
        args.appfs_session.clone(),
        AppfsBridgeCliArgs {
            adapter_http_endpoint: args.adapter_http_endpoint.clone(),
            adapter_http_timeout_ms: args.adapter_http_timeout_ms,
            adapter_grpc_endpoint: args.adapter_grpc_endpoint.clone(),
            adapter_grpc_timeout_ms: args.adapter_grpc_timeout_ms,
            adapter_bridge_max_retries: args.adapter_bridge_max_retries,
            adapter_bridge_initial_backoff_ms: args.adapter_bridge_initial_backoff_ms,
            adapter_bridge_max_backoff_ms: args.adapter_bridge_max_backoff_ms,
            adapter_bridge_circuit_breaker_failures: args.adapter_bridge_circuit_breaker_failures,
            adapter_bridge_circuit_breaker_cooldown_ms: args
                .adapter_bridge_circuit_breaker_cooldown_ms,
        },
        None,
    )
}

#[cfg(any(unix, target_os = "windows"))]
fn wrap_mount_fs_if_appfs_enabled(
    fs: Arc<Mutex<dyn FileSystem + Send>>,
    args: &MountArgs,
) -> Result<Arc<Mutex<dyn FileSystem + Send>>> {
    let runtime_args = appfs_mount_runtime_args(args)?;
    if runtime_args.is_empty() {
        return Ok(fs);
    }
    let bridge_config = appfs::build_appfs_bridge_config(AppfsBridgeCliArgs {
        adapter_http_endpoint: args.adapter_http_endpoint.clone(),
        adapter_http_timeout_ms: args.adapter_http_timeout_ms,
        adapter_grpc_endpoint: args.adapter_grpc_endpoint.clone(),
        adapter_grpc_timeout_ms: args.adapter_grpc_timeout_ms,
        adapter_bridge_max_retries: args.adapter_bridge_max_retries,
        adapter_bridge_initial_backoff_ms: args.adapter_bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms: args.adapter_bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures: args.adapter_bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms: args.adapter_bridge_circuit_breaker_cooldown_ms,
    });
    Ok(appfs::mount_readthrough::wrap_mount_readthrough_filesystem(
        fs,
        appfs::mount_readthrough::MountSnapshotReadThroughConfig {
            runtimes: runtime_args,
            bridge_config,
        },
    ))
}

#[cfg(target_os = "linux")]
struct DaemonMutexFsAdapter {
    inner: Arc<Mutex<dyn FileSystem + Send>>,
}

#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl FileSystem for DaemonMutexFsAdapter {
    async fn lookup(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.lookup(parent_ino, name).await
    }

    async fn getattr(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.getattr(ino).await
    }

    async fn readlink(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<String>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readlink(ino).await
    }

    async fn readdir(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<String>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir(ino).await
    }

    async fn readdir_plus(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<agentfs_sdk::DirEntry>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir_plus(ino).await
    }

    async fn chmod(
        &self,
        ino: i64,
        mode: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chmod(ino, mode).await
    }

    async fn chown(
        &self,
        ino: i64,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chown(ino, uid, gid).await
    }

    async fn utimens(
        &self,
        ino: i64,
        atime: agentfs_sdk::TimeChange,
        mtime: agentfs_sdk::TimeChange,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.utimens(ino, atime, mtime).await
    }

    async fn open(
        &self,
        ino: i64,
        flags: i32,
    ) -> std::result::Result<agentfs_sdk::BoxedFile, agentfs_sdk::error::Error> {
        self.inner.lock().await.open(ino, flags).await
    }

    async fn mkdir(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .mkdir(parent_ino, name, mode, uid, gid)
            .await
    }

    async fn create_file(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(agentfs_sdk::Stats, agentfs_sdk::BoxedFile), agentfs_sdk::error::Error>
    {
        self.inner
            .lock()
            .await
            .create_file(parent_ino, name, mode, uid, gid)
            .await
    }

    async fn mknod(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        rdev: u64,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .mknod(parent_ino, name, mode, rdev, uid, gid)
            .await
    }

    async fn symlink(
        &self,
        parent_ino: i64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .symlink(parent_ino, name, target, uid, gid)
            .await
    }

    async fn unlink(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.unlink(parent_ino, name).await
    }

    async fn rmdir(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.rmdir(parent_ino, name).await
    }

    async fn link(
        &self,
        ino: i64,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .link(ino, newparent_ino, newname)
            .await
    }

    async fn rename(
        &self,
        oldparent_ino: i64,
        oldname: &str,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .rename(oldparent_ino, oldname, newparent_ino, newname)
            .await
    }

    async fn statfs(
        &self,
    ) -> std::result::Result<agentfs_sdk::FilesystemStats, agentfs_sdk::error::Error> {
        self.inner.lock().await.statfs().await
    }
}

#[cfg(target_os = "windows")]
struct WinfspTokioFsAdapter {
    inner: Arc<Mutex<dyn FileSystem + Send>>,
}

#[cfg(target_os = "windows")]
#[async_trait::async_trait]
impl FileSystem for WinfspTokioFsAdapter {
    async fn lookup(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.lookup(parent_ino, name).await
    }

    async fn getattr(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<agentfs_sdk::Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.getattr(ino).await
    }

    async fn readlink(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<String>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readlink(ino).await
    }

    async fn readdir(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<String>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir(ino).await
    }

    async fn readdir_plus(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<agentfs_sdk::DirEntry>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir_plus(ino).await
    }

    async fn chmod(
        &self,
        ino: i64,
        mode: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chmod(ino, mode).await
    }

    async fn chown(
        &self,
        ino: i64,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chown(ino, uid, gid).await
    }

    async fn utimens(
        &self,
        ino: i64,
        atime: agentfs_sdk::TimeChange,
        mtime: agentfs_sdk::TimeChange,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.utimens(ino, atime, mtime).await
    }

    async fn open(
        &self,
        ino: i64,
        flags: i32,
    ) -> std::result::Result<agentfs_sdk::BoxedFile, agentfs_sdk::error::Error> {
        self.inner.lock().await.open(ino, flags).await
    }

    async fn mkdir(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .mkdir(parent_ino, name, mode, uid, gid)
            .await
    }

    async fn create_file(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(agentfs_sdk::Stats, agentfs_sdk::BoxedFile), agentfs_sdk::error::Error>
    {
        self.inner
            .lock()
            .await
            .create_file(parent_ino, name, mode, uid, gid)
            .await
    }

    async fn mknod(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        rdev: u64,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .mknod(parent_ino, name, mode, rdev, uid, gid)
            .await
    }

    async fn symlink(
        &self,
        parent_ino: i64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .symlink(parent_ino, name, target, uid, gid)
            .await
    }

    async fn unlink(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.unlink(parent_ino, name).await
    }

    async fn rmdir(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.rmdir(parent_ino, name).await
    }

    async fn link(
        &self,
        ino: i64,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<agentfs_sdk::Stats, agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .link(ino, newparent_ino, newname)
            .await
    }

    async fn rename(
        &self,
        oldparent_ino: i64,
        oldname: &str,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .rename(oldparent_ino, oldname, newparent_ino, newname)
            .await
    }

    async fn statfs(
        &self,
    ) -> std::result::Result<agentfs_sdk::FilesystemStats, agentfs_sdk::error::Error> {
        self.inner.lock().await.statfs().await
    }
}

/// Mount the agent filesystem (Linux).
#[cfg(target_os = "linux")]
pub fn mount(args: MountArgs) -> Result<()> {
    if has_appfs_mount_config(&args) && args.appfs_app_id.is_none() {
        anyhow::bail!(
            "AppFS mount-side read-through requires --appfs-app-id when any AppFS bridge option is provided."
        );
    }
    if has_appfs_mount_config(&args) && !matches!(args.backend, MountBackend::Fuse) {
        anyhow::bail!(
            "AppFS snapshot read-through is currently supported only with --backend fuse on Linux."
        );
    }
    match args.backend {
        MountBackend::Fuse => mount_fuse(args),
        MountBackend::Nfs => {
            let rt = crate::get_runtime();
            rt.block_on(mount_nfs_backend(args))
        }
        MountBackend::Winfsp => anyhow::bail!(
            "WinFsp mounting is only supported on Windows.\n\
             Use --backend fuse or --backend nfs on Linux."
        ),
    }
}

/// Mount the agent filesystem (macOS).
#[cfg(target_os = "macos")]
pub fn mount(args: MountArgs) -> Result<()> {
    if has_appfs_mount_config(&args) && args.appfs_app_id.is_none() {
        anyhow::bail!(
            "AppFS mount-side read-through requires --appfs-app-id when any AppFS bridge option is provided."
        );
    }
    match args.backend {
        MountBackend::Fuse => {
            anyhow::bail!(
                "FUSE mounting is not supported on macOS.\n\
                 Use --backend nfs (default) or `agentfs nfs` instead."
            );
        }
        MountBackend::Nfs => {
            let rt = crate::get_runtime();
            rt.block_on(mount_nfs_backend(args))
        }
        MountBackend::Winfsp => {
            anyhow::bail!(
                "WinFsp mounting is only supported on Windows.\n\
                 Use --backend nfs on macOS."
            );
        }
    }
}

/// Mount the agent filesystem (Windows).
#[cfg(target_os = "windows")]
pub fn mount(args: MountArgs) -> Result<()> {
    if has_appfs_mount_config(&args) && args.appfs_app_id.is_none() {
        anyhow::bail!(
            "AppFS mount-side read-through requires --appfs-app-id when any AppFS bridge option is provided."
        );
    }
    match args.backend {
        MountBackend::Fuse => {
            anyhow::bail!(
                "FUSE mounting is not supported on Windows.\n\
                 Use --backend winfsp instead."
            );
        }
        MountBackend::Nfs => {
            anyhow::bail!(
                "NFS mounting is not supported on Windows.\n\
                 Use --backend winfsp instead."
            );
        }
        MountBackend::Winfsp => {
            let rt = crate::get_runtime();
            rt.block_on(mount_winfsp_backend(args))
        }
    }
}

/// Mount the agent filesystem using WinFsp (Windows only).
#[cfg(target_os = "windows")]
async fn mount_winfsp_backend(args: MountArgs) -> Result<()> {
    use parking_lot::Mutex;

    let opts = AgentFSOptions::resolve(&args.id_or_path)?;

    // WinFsp requires the mountpoint to NOT exist - it will create the directory itself.
    // This is opposite to FUSE/NFS which require the directory to exist.
    if args.mountpoint.exists() {
        anyhow::bail!(
            "Mountpoint already exists: {}. WinFsp requires a non-existent path - it will create the directory.",
            args.mountpoint.display()
        );
    }
    let mount_parent = args
        .mountpoint
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "WinFsp mountpoint must include an existing parent directory: {}",
                args.mountpoint.display()
            )
        })?;
    if !mount_parent.exists() {
        anyhow::bail!(
            "WinFsp mountpoint parent does not exist: {}",
            mount_parent.display()
        );
    }
    if !mount_parent.is_dir() {
        anyhow::bail!(
            "WinFsp mountpoint parent is not a directory: {}",
            mount_parent.display()
        );
    }

    // Don't use canonicalize - it adds the \\?\ prefix which WinFsp doesn't accept,
    // and it would fail anyway if the path doesn't exist.
    let mountpoint = args.mountpoint.clone();

    // WinFsp FileSystemName field is limited to 16 WCHARs, so use just "agentfs"
    let fsname = "agentfs".to_string();

    // Open AgentFS
    let agentfs = match open_agentfs(opts).await {
        Ok(fs) => fs,
        Err(SdkError::SchemaVersionMismatch { found, expected }) => {
            exit_schema_version_mismatch(&found, &expected, &args.id_or_path);
        }
        Err(e) => return Err(e.into()),
    };
    hydrate_windows_overlay_base(&agentfs).await?;

    let fs: Arc<tokio::sync::Mutex<dyn agentfs_sdk::FileSystem + Send>> =
        Arc::new(tokio::sync::Mutex::new(agentfs.fs));
    let fs = wrap_mount_fs_if_appfs_enabled(fs, &args)?;
    let fs: Arc<Mutex<dyn agentfs_sdk::FileSystem + Send>> =
        Arc::new(Mutex::new(WinfspTokioFsAdapter { inner: fs }));

    let mount_opts = MountOpts {
        mountpoint: mountpoint.clone(),
        backend: MountBackend::Winfsp,
        fsname,
        uid: args.uid,
        gid: args.gid,
        allow_other: args.allow_other,
        allow_root: args.allow_root,
        auto_unmount: args.auto_unmount,
        lazy_unmount: false,
        timeout: std::time::Duration::from_secs(10),
    };

    // Call winfsp mount directly
    let _mount_handle = crate::mount::winfsp::mount_winfsp(fs, mount_opts).await?;

    eprintln!("Mounted at {}", mountpoint.display());
    eprintln!("Press Ctrl+C to unmount and exit.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    // MountHandle will be dropped automatically and unmount
    Ok(())
}

#[cfg(target_os = "windows")]
async fn hydrate_windows_overlay_base(agentfs: &agentfs_sdk::AgentFS) -> Result<()> {
    let Some(base_path) = agentfs.is_overlay_enabled().await? else {
        return Ok(());
    };

    let whiteouts = agentfs.get_whiteouts().await?;
    let base_root = PathBuf::from(&base_path);
    if !base_root.exists() {
        anyhow::bail!(
            "Overlay base directory does not exist: {}",
            base_root.display()
        );
    }

    eprintln!(
        "Hydrating overlay base into WinFsp delta view from: {}",
        base_root.display()
    );

    let mut pending_dirs = vec![base_root.clone()];
    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read base directory {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("Failed to enumerate {}", dir.display()))?;
            let host_path = entry.path();
            let rel = host_path.strip_prefix(&base_root).with_context(|| {
                format!(
                    "Failed to compute base-relative path for {} under {}",
                    host_path.display(),
                    base_root.display()
                )
            })?;
            if rel.as_os_str().is_empty() {
                continue;
            }

            let rel_slash = rel.to_string_lossy().replace('\\', "/");
            let fs_path = format!("/{}", rel_slash);
            if path_is_whiteouted(&fs_path, &whiteouts) {
                continue;
            }

            let metadata = std::fs::symlink_metadata(&host_path)
                .with_context(|| format!("Failed to stat {}", host_path.display()))?;
            let file_type = metadata.file_type();

            if file_type.is_dir() {
                ensure_agentfs_parent_dirs(agentfs, &fs_path).await?;
                match agentfs.fs.stat(&fs_path).await? {
                    Some(stats) if !stats.is_directory() => {
                        anyhow::bail!(
                            "Cannot hydrate directory {} because a non-directory already exists in delta",
                            fs_path
                        );
                    }
                    None => {
                        agentfs.fs.mkdir(&fs_path, 0, 0).await?;
                    }
                    _ => {}
                }
                pending_dirs.push(host_path);
                continue;
            }

            ensure_agentfs_parent_dirs(agentfs, &fs_path).await?;
            if let Some(existing) = agentfs.fs.stat(&fs_path).await? {
                if file_type.is_file() && existing.is_file() {
                    continue;
                }
                if file_type.is_symlink() && existing.is_symlink() {
                    continue;
                }
                continue;
            }

            if file_type.is_symlink() {
                let target = std::fs::read_link(&host_path)
                    .with_context(|| format!("Failed to read symlink {}", host_path.display()))?;
                let target = target.to_string_lossy().replace('\\', "/");
                agentfs.fs.symlink(&target, &fs_path, 0, 0).await?;
                continue;
            }

            if file_type.is_file() {
                let bytes = std::fs::read(&host_path)
                    .with_context(|| format!("Failed to read file {}", host_path.display()))?;
                let mode = if metadata.permissions().readonly() {
                    0o444
                } else {
                    0o644
                };
                let (_, file) = agentfs.fs.create_file(&fs_path, mode, 0, 0).await?;
                if !bytes.is_empty() {
                    file.pwrite(0, &bytes).await?;
                }
                file.fsync().await?;
            }
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
async fn ensure_agentfs_parent_dirs(agentfs: &agentfs_sdk::AgentFS, fs_path: &str) -> Result<()> {
    let path = Path::new(fs_path);
    let mut current = PathBuf::new();
    let mut components = path.components().peekable();

    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        current.push(component.as_os_str());
        let dir_path = current.to_string_lossy().replace('\\', "/");
        if dir_path.is_empty() || dir_path == "/" {
            continue;
        }
        match agentfs.fs.stat(&dir_path).await? {
            Some(stats) if !stats.is_directory() => {
                anyhow::bail!("Parent path exists but is not a directory: {}", dir_path);
            }
            None => {
                agentfs.fs.mkdir(&dir_path, 0, 0).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn path_is_whiteouted(path: &str, whiteouts: &HashSet<String>) -> bool {
    let mut current = path.trim_end_matches('/');
    loop {
        if whiteouts.contains(current) {
            return true;
        }
        if current.is_empty() || current == "/" {
            return false;
        }
        current = current
            .rsplit_once('/')
            .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
            .unwrap_or("/");
    }
}

/// Mount the agent filesystem using FUSE (Linux only).
#[cfg(target_os = "linux")]
fn mount_fuse(args: MountArgs) -> Result<()> {
    let opts = AgentFSOptions::resolve(&args.id_or_path)?;

    // Check schema version before daemonizing. This allows us to show the error
    // message to the user directly, rather than having it appear in daemon logs.
    {
        let rt = crate::get_runtime();
        let db_path = opts.db_path()?;
        let result: Result<(), SdkError> = rt.block_on(async {
            let db = turso::Builder::new_local(&db_path).build().await?;
            let conn = db.connect()?;
            agentfs_sdk::schema::check_schema_version(&conn).await?;
            Ok(())
        });
        if let Err(SdkError::SchemaVersionMismatch { found, expected }) = result {
            exit_schema_version_mismatch(&found, &expected, &args.id_or_path);
        }
    }

    let fsname = format!(
        "agentfs:{}",
        std::fs::canonicalize(&args.id_or_path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| args.id_or_path.clone())
    );

    let mountpoint = canonicalize_mountpoint_with_recovery(&args.mountpoint)?;
    let mountpoint_ino = {
        use anyhow::Context as _;
        std::fs::metadata(mountpoint.clone())
            .context("Failed to get mountpoint inode")?
            .ino()
    };

    let fuse_opts = FuseMountOptions {
        mountpoint: args.mountpoint.clone(),
        auto_unmount: args.auto_unmount,
        allow_root: args.allow_root,
        allow_other: args.allow_other,
        fsname: fsname.clone(),
        uid: args.uid,
        gid: args.gid,
    };

    if args.foreground {
        let rt = crate::get_runtime();
        let agentfs = match rt.block_on(open_agentfs(opts)) {
            Ok(fs) => fs,
            Err(SdkError::SchemaVersionMismatch { found, expected }) => {
                exit_schema_version_mismatch(&found, &expected, &args.id_or_path);
            }
            Err(e) => return Err(e.into()),
        };

        // Check for overlay configuration
        let fs: Arc<Mutex<dyn FileSystem + Send>> = rt.block_on(async {
            // Query base_path in a separate scope so connection is released before overlay load.
            let base_path: Option<String> = {
                let conn = agentfs.get_connection().await?;
                let query = "SELECT value FROM fs_overlay_config WHERE key = 'base_path'";
                match conn.query(query, ()).await {
                    Ok(mut rows) => {
                        if let Ok(Some(row)) = rows.next().await {
                            row.get_value(0).ok().and_then(|v| {
                                if let Value::Text(s) = v {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    }
                    Err(_) => None, // Table doesn't exist or query failed
                }
            }; // conn is dropped here

            if let Some(base_path) = base_path {
                // Create OverlayFS with HostFS base, loading existing whiteouts
                eprintln!("Using overlay filesystem with base: {}", base_path);
                let hostfs = HostFS::new(&base_path)?;
                let hostfs = hostfs.with_fuse_mountpoint(mountpoint_ino);
                let overlay = OverlayFS::new(Arc::new(hostfs), agentfs.fs);
                overlay.load().await?; // Load persisted whiteouts and origin mappings
                Ok::<Arc<Mutex<dyn FileSystem + Send>>, anyhow::Error>(Arc::new(Mutex::new(
                    overlay,
                )))
            } else {
                // Plain AgentFS
                Ok(Arc::new(Mutex::new(agentfs.fs)) as Arc<Mutex<dyn FileSystem + Send>>)
            }
        })?;
        let fs = wrap_mount_fs_if_appfs_enabled(fs, &args)?;

        let mount_opts = MountOpts {
            mountpoint: mountpoint.clone(),
            backend: MountBackend::Fuse,
            fsname: fsname.clone(),
            uid: args.uid,
            gid: args.gid,
            allow_other: args.allow_other,
            allow_root: args.allow_root,
            auto_unmount: args.auto_unmount,
            // Prefer lazy detach on Ctrl+C path to reduce stale mountpoint risk.
            lazy_unmount: true,
            timeout: std::time::Duration::from_secs(10),
        };

        rt.block_on(async {
            let _mount_handle = mount_fs(fs, mount_opts).await?;
            eprintln!("Mounted at {}", mountpoint.display());
            eprintln!("Press Ctrl+C to unmount and exit.");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = wait_for_external_unmount(mountpoint.clone()) => {
                    eprintln!(
                        "Mountpoint {} was unmounted externally; exiting foreground mode.",
                        mountpoint.display()
                    );
                }
            }
            Ok::<(), anyhow::Error>(())
        })?;

        return Ok(());
    }

    let opts = AgentFSOptions::resolve(&args.id_or_path)?;
    let id_or_path = args.id_or_path.clone();
    let mount = move || {
        let rt = crate::get_runtime();
        let agentfs = match rt.block_on(open_agentfs(opts)) {
            Ok(fs) => fs,
            Err(SdkError::SchemaVersionMismatch { found, expected }) => {
                exit_schema_version_mismatch(&found, &expected, &id_or_path);
            }
            Err(e) => return Err(e.into()),
        };

        // Check for overlay configuration
        let fs: Arc<Mutex<dyn FileSystem + Send>> = rt.block_on(async {
            // Query base_path in a separate scope so connection is released
            let base_path: Option<String> = {
                let conn = agentfs.get_connection().await?;
                let query = "SELECT value FROM fs_overlay_config WHERE key = 'base_path'";
                match conn.query(query, ()).await {
                    Ok(mut rows) => {
                        if let Ok(Some(row)) = rows.next().await {
                            row.get_value(0).ok().and_then(|v| {
                                if let Value::Text(s) = v {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    }
                    Err(_) => None, // Table doesn't exist or query failed
                }
            }; // conn is dropped here

            if let Some(base_path) = base_path {
                // Create OverlayFS with HostFS base, loading existing whiteouts
                eprintln!("Using overlay filesystem with base: {}", base_path);
                let hostfs = HostFS::new(&base_path)?;
                let hostfs = hostfs.with_fuse_mountpoint(mountpoint_ino);
                let overlay = OverlayFS::new(Arc::new(hostfs), agentfs.fs);
                overlay.load().await?; // Load persisted whiteouts and origin mappings
                Ok::<Arc<Mutex<dyn FileSystem + Send>>, anyhow::Error>(Arc::new(Mutex::new(
                    overlay,
                )))
            } else {
                // Plain AgentFS
                Ok(Arc::new(Mutex::new(agentfs.fs)) as Arc<Mutex<dyn FileSystem + Send>>)
            }
        })?;
        let fs = wrap_mount_fs_if_appfs_enabled(fs, &args)?;
        let fs_adapter = DaemonMutexFsAdapter { inner: fs };
        let fs_arc: Arc<dyn FileSystem> = Arc::new(fs_adapter);

        crate::fuse::mount(fs_arc, fuse_opts, rt)
    };

    crate::daemon::daemonize(
        mount,
        move || is_mounted(&mountpoint),
        std::time::Duration::from_secs(10),
    )
}

#[cfg(target_os = "linux")]
fn canonicalize_mountpoint_with_recovery(mountpoint: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(mountpoint) {
        Ok(path) => Ok(path),
        Err(err) if err.raw_os_error() == Some(libc::ENOTCONN) => {
            eprintln!(
                "Detected stale FUSE mountpoint state at {} (ENOTCONN); attempting lazy unmount recovery.",
                mountpoint.display()
            );
            lazy_unmount_fuse(mountpoint)?;
            std::fs::canonicalize(mountpoint).with_context(|| {
                format!(
                    "Mountpoint {} is still unavailable after stale mountpoint recovery",
                    mountpoint.display()
                )
            })
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Mountpoint does not exist: {}", mountpoint.display());
        }
        Err(err) => Err(err)
            .with_context(|| format!("Failed to access mountpoint {}", mountpoint.display())),
    }
}

#[cfg(target_os = "linux")]
fn lazy_unmount_fuse(mountpoint: &Path) -> Result<()> {
    const FUSERMOUNT_COMMANDS: &[&str] = &["fusermount3", "fusermount"];
    for cmd in FUSERMOUNT_COMMANDS {
        let result = std::process::Command::new(cmd)
            .args(["-uz"])
            .arg(mountpoint.as_os_str())
            .status();

        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,  // Command ran but failed, try next.
            Err(_) => continue, // Command not found, try next.
        }
    }

    anyhow::bail!(
        "Failed to auto-recover stale mountpoint {}. Try: fusermount -uz {}",
        mountpoint.display(),
        mountpoint.display()
    )
}

/// Mount the agent filesystem using NFS over localhost.
#[cfg(unix)]
async fn mount_nfs_backend(args: MountArgs) -> Result<()> {
    use crate::cmd::init::open_agentfs;

    let opts = AgentFSOptions::resolve(&args.id_or_path)?;

    if !args.mountpoint.exists() {
        anyhow::bail!("Mountpoint does not exist: {}", args.mountpoint.display());
    }

    let mountpoint = std::fs::canonicalize(args.mountpoint.clone())?;

    let fsname = format!(
        "agentfs:{}",
        std::fs::canonicalize(&args.id_or_path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| args.id_or_path.clone())
    );

    // Open AgentFS
    let agentfs = match open_agentfs(opts).await {
        Ok(fs) => fs,
        Err(SdkError::SchemaVersionMismatch { found, expected }) => {
            exit_schema_version_mismatch(&found, &expected, &args.id_or_path);
        }
        Err(e) => return Err(e.into()),
    };

    // Check for overlay configuration
    // Query base_path in a separate scope so connection is released before load_whiteouts
    let base_path: Option<String> = {
        let conn = agentfs.get_connection().await?;
        let query = "SELECT value FROM fs_overlay_config WHERE key = 'base_path'";
        match conn.query(query, ()).await {
            Ok(mut rows) => {
                if let Ok(Some(row)) = rows.next().await {
                    row.get_value(0).ok().and_then(|v| {
                        if let Value::Text(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            }
            Err(_) => None, // Table doesn't exist or query failed
        }
    }; // conn is dropped here

    let fs: Arc<Mutex<dyn FileSystem + Send>> = if let Some(base_path) = base_path {
        // Create OverlayFS with HostFS base, loading existing whiteouts
        eprintln!("Using overlay filesystem with base: {}", base_path);
        let hostfs = HostFS::new(&base_path)?;
        let overlay = OverlayFS::new(Arc::new(hostfs), agentfs.fs);
        overlay.load().await?; // Load persisted whiteouts and origin mappings
        Arc::new(Mutex::new(overlay)) as Arc<Mutex<dyn FileSystem + Send>>
    } else {
        // Plain AgentFS
        Arc::new(Mutex::new(agentfs.fs)) as Arc<Mutex<dyn FileSystem + Send>>
    };
    let fs = wrap_mount_fs_if_appfs_enabled(fs, &args)?;

    if args.foreground {
        // Use the unified mount API for foreground mode
        let mount_opts = MountOpts {
            mountpoint: mountpoint.clone(),
            backend: MountBackend::Nfs,
            fsname,
            uid: args.uid,
            gid: args.gid,
            allow_other: args.allow_other,
            allow_root: args.allow_root,
            auto_unmount: args.auto_unmount,
            lazy_unmount: true,
            timeout: std::time::Duration::from_secs(10),
        };

        let _mount_handle = mount_fs(fs, mount_opts).await?;

        eprintln!("Mounted at {}", mountpoint.display());
        eprintln!("Press Ctrl+C to unmount and exit.");
        tokio::signal::ctrl_c().await?;

        // Handle drops automatically when we exit this scope
    } else {
        // Daemon mode: use manual NFS server setup for persistent background operation
        let nfs = AgentNFS::new(fs);
        let port = find_available_port(DEFAULT_NFS_PORT)?;

        let bind_addr = format!("127.0.0.1:{}", port);
        let listener = crate::nfsserve::tcp::NFSTcpListener::bind(&bind_addr, nfs)
            .await
            .context("Failed to bind NFS server")?;

        eprintln!("Starting NFS server on 127.0.0.1:{}", port);

        tokio::spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                eprintln!("NFS server error: {}", e);
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        nfs_mount(port, &mountpoint)?;

        eprintln!("Mounted at {}", mountpoint.display());
        eprintln!(
            "Running in background. Use 'umount {}' to unmount.",
            mountpoint.display()
        );

        // Block forever (server runs in background task)
        std::future::pending::<()>().await;
    }

    Ok(())
}

/// Find an available TCP port starting from the given port.
#[cfg(unix)]
fn find_available_port(start_port: u32) -> Result<u32> {
    for port in start_port..start_port + 100 {
        if std::net::TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return Ok(port);
        }
    }
    anyhow::bail!(
        "Could not find an available port in range {}-{}",
        start_port,
        start_port + 100
    );
}

/// Mount the NFS filesystem (Linux version).
#[cfg(target_os = "linux")]
fn nfs_mount(port: u32, mountpoint: &Path) -> Result<()> {
    let output = Command::new("mount")
        .args([
            "-t",
            "nfs",
            "-o",
            &format!(
                "vers=3,tcp,port={},mountport={},nolock,soft,timeo=10,retrans=2",
                port, port
            ),
            "127.0.0.1:/",
            mountpoint.to_str().unwrap(),
        ])
        .output()
        .context("Failed to execute mount command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Failed to mount NFS: {}. Make sure NFS client tools are installed (nfs-common on Debian/Ubuntu, nfs-utils on Fedora/RHEL) and you have permission to mount (try running with sudo).",
            stderr.trim()
        );
    }

    Ok(())
}

/// Mount the NFS filesystem (macOS version).
#[cfg(target_os = "macos")]
fn nfs_mount(port: u32, mountpoint: &Path) -> Result<()> {
    let output = Command::new("/sbin/mount_nfs")
        .args([
            "-o",
            &format!(
                "locallocks,vers=3,tcp,port={},mountport={},soft,timeo=10,retrans=2",
                port, port
            ),
            "127.0.0.1:/",
            mountpoint.to_str().unwrap(),
        ])
        .output()
        .context("Failed to execute mount_nfs")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to mount NFS: {}", stderr.trim());
    }

    Ok(())
}

/// Check if a path is a mountpoint by comparing device IDs
#[cfg(target_os = "linux")]
fn is_mounted(path: &std::path::Path) -> bool {
    let path_meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("/"),
    };

    let parent_meta = match std::fs::metadata(parent) {
        Ok(m) => m,
        Err(_) => return false,
    };

    // Different device IDs means it's a mountpoint
    path_meta.dev() != parent_meta.dev()
}

/// Wait until a mounted path is externally unmounted.
///
/// This keeps foreground mount mode compatible with test harnesses that
/// unmount using `fusermount -u` and then wait for the foreground process.
#[cfg(target_os = "linux")]
async fn wait_for_external_unmount(mountpoint: PathBuf) {
    let mut consecutive_not_mounted = 0u8;
    loop {
        if is_mounted(&mountpoint) {
            consecutive_not_mounted = 0;
        } else {
            consecutive_not_mounted = consecutive_not_mounted.saturating_add(1);
            // Require multiple consecutive misses to avoid transient mount-state races.
            if consecutive_not_mounted >= 3 {
                return;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}

/// List all currently mounted agentfs filesystems (Linux)
#[cfg(target_os = "linux")]
pub fn list_mounts<W: Write>(out: &mut W) {
    let mounts = get_mounts();

    if mounts.is_empty() {
        let _ = writeln!(out, "No agentfs filesystems mounted.");
        return;
    }

    // Calculate column widths
    let id_width = mounts.iter().map(|m| m.id.len()).max().unwrap_or(2).max(2);
    let mount_width = mounts
        .iter()
        .map(|m| m.mountpoint.to_string_lossy().len())
        .max()
        .unwrap_or(10)
        .max(10);

    // Print header
    let _ = writeln!(
        out,
        "{:<id_width$}  {:<mount_width$}",
        "ID",
        "MOUNTPOINT",
        id_width = id_width,
        mount_width = mount_width
    );

    // Print mounts
    for mount in &mounts {
        let _ = writeln!(
            out,
            "{:<id_width$}  {:<mount_width$}",
            mount.id,
            mount.mountpoint.display(),
            id_width = id_width,
            mount_width = mount_width
        );
    }
}

/// List all currently mounted agentfs filesystems (macOS stub)
#[cfg(target_os = "macos")]
pub fn list_mounts<W: std::io::Write>(out: &mut W) {
    let _ = writeln!(out, "Mount listing is only available on Linux.");
}

/// Check if a mount point is in use by any process.
///
/// Scans /proc to find processes with open files or current working directory
/// on the given mountpoint.
#[cfg(target_os = "linux")]
fn is_mount_in_use(mountpoint: &Path) -> bool {
    let mountpoint = match mountpoint.canonicalize() {
        Ok(p) => p,
        Err(_) => return false, // Can't check, assume not in use
    };

    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(dir) => dir,
        Err(_) => return false,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only check numeric directories (PIDs)
        if !name_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let pid_path = entry.path();

        // Check cwd
        if let Ok(cwd) = std::fs::read_link(pid_path.join("cwd")) {
            if cwd.starts_with(&mountpoint) {
                return true;
            }
        }

        // Check open file descriptors
        let fd_dir = pid_path.join("fd");
        if let Ok(fds) = std::fs::read_dir(&fd_dir) {
            for fd_entry in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd_entry.path()) {
                    if target.starts_with(&mountpoint) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Unmount a FUSE filesystem.
///
/// Tries fusermount3 first, then falls back to fusermount.
#[cfg(target_os = "linux")]
fn unmount_fuse(mountpoint: &Path) -> Result<()> {
    const FUSERMOUNT_COMMANDS: &[&str] = &["fusermount3", "fusermount"];

    for cmd in FUSERMOUNT_COMMANDS {
        let result = std::process::Command::new(cmd)
            .args(["-u"])
            .arg(mountpoint.as_os_str())
            .status();

        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,  // Command ran but failed, try next
            Err(_) => continue, // Command not found, try next
        }
    }

    anyhow::bail!(
        "Failed to unmount {}. You may need to unmount manually with: fusermount -u {}",
        mountpoint.display(),
        mountpoint.display()
    )
}

/// Ask for user confirmation.
#[cfg(target_os = "linux")]
fn confirm(prompt: &str) -> bool {
    eprint!("{} ", prompt);
    let _ = io::stderr().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Prune unused agentfs mount points.
///
/// Finds all mounted agentfs filesystems that are not in use by any process
/// and unmounts them.
#[cfg(target_os = "linux")]
pub fn prune_mounts(force: bool) -> Result<()> {
    let mounts = get_mounts();

    // Get active session IDs to exclude from pruning
    let active_sessions = super::ps::active_session_ids();

    // Find unused mounts (not in use by any process and no active session)
    let unused_mounts: Vec<&Mount> = mounts
        .iter()
        .filter(|m| !is_mount_in_use(&m.mountpoint) && !active_sessions.contains(&m.id))
        .collect();

    if unused_mounts.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }

    // Display what will be unmounted
    println!("The following unused mount points will be unmounted:");
    println!();
    for mount in &unused_mounts {
        println!("  {} -> {}", mount.id, mount.mountpoint.display());
    }
    println!();

    // Ask for confirmation unless --force
    if !force && !confirm("Are you sure? (y/N)") {
        println!("Aborted.");
        return Ok(());
    }

    // Unmount each unused mount
    let mut errors = Vec::new();
    for mount in &unused_mounts {
        print!("Unmounting {}... ", mount.mountpoint.display());
        let _ = io::stdout().flush();

        match unmount_fuse(&mount.mountpoint) {
            Ok(()) => println!("done"),
            Err(e) => {
                println!("failed");
                errors.push(format!("{}: {}", mount.mountpoint.display(), e));
            }
        }
    }

    if !errors.is_empty() {
        eprintln!();
        eprintln!("Some mounts could not be unmounted:");
        for error in &errors {
            eprintln!("  {}", error);
        }
        anyhow::bail!("Failed to unmount {} mount(s)", errors.len());
    }

    Ok(())
}

/// Prune unused agentfs mount points (macOS stub).
#[cfg(target_os = "macos")]
pub fn prune_mounts(_force: bool) -> Result<()> {
    anyhow::bail!("Mount pruning is only available on Linux")
}

/// List all currently mounted agentfs filesystems (Windows stub).
#[cfg(target_os = "windows")]
pub fn list_mounts<W: std::io::Write>(out: &mut W) {
    let _ = writeln!(out, "Mount listing is only available on Linux.");
}

/// Prune unused agentfs mount points (Windows stub).
#[cfg(target_os = "windows")]
pub fn prune_mounts(_force: bool) -> Result<()> {
    anyhow::bail!("Mount pruning is only available on Linux")
}

#[cfg(all(test, target_os = "windows"))]
mod windows_overlay_tests {
    use super::hydrate_windows_overlay_base;
    use agentfs_sdk::{agentfs_dir, AgentFS, AgentFSOptions};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).expect("create destination dir");
        for entry in fs::read_dir(src).expect("read source dir") {
            let entry = entry.expect("dir entry");
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().expect("file type").is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                fs::copy(&src_path, &dst_path).expect("copy file");
            }
        }
    }

    fn cleanup_agent(id: &str) {
        for suffix in [".db", ".db-shm", ".db-wal"] {
            let _ = fs::remove_file(agentfs_dir().join(format!("{id}{suffix}")));
        }
    }

    #[tokio::test]
    async fn winfsp_overlay_hydration_materializes_base_tree_without_overwriting_delta() {
        let fixture_root = TempDir::new().expect("fixture tempdir");
        let app_dir = fixture_root.path().join("aiim");
        let source_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/appfs/aiim");
        copy_dir_recursive(&source_fixture, &app_dir);

        let id = format!("winfsp-hydrate-{}", uuid::Uuid::new_v4().simple());
        let agent = AgentFS::open(AgentFSOptions::with_id(&id).with_base(fixture_root.path()))
            .await
            .expect("open overlay agent");

        agent.fs.mkdir("/aiim", 0, 0).await.expect("seed app dir");
        agent
            .fs
            .mkdir("/aiim/_meta", 0, 0)
            .await
            .expect("seed meta dir");
        let (_, file) = agent
            .fs
            .create_file("/aiim/custom.txt", 0o644, 0, 0)
            .await
            .expect("seed delta file");
        file.pwrite(0, b"delta-only")
            .await
            .expect("write delta file");
        file.fsync().await.expect("fsync delta file");

        let (_, manifest_override) = agent
            .fs
            .create_file("/aiim/_meta/manifest.res.json", 0o644, 0, 0)
            .await
            .expect("create override manifest");
        manifest_override
            .pwrite(0, b"{\"override\":true}\n")
            .await
            .expect("write override manifest");
        manifest_override
            .fsync()
            .await
            .expect("fsync override manifest");

        hydrate_windows_overlay_base(&agent)
            .await
            .expect("hydrate overlay base");

        assert!(
            agent
                .fs
                .stat("/aiim/chats/chat-001/messages.res.jsonl")
                .await
                .expect("stat hydrated snapshot")
                .is_some(),
            "base snapshot should be materialized into delta"
        );
        let manifest = agent
            .fs
            .read_file("/aiim/_meta/manifest.res.json")
            .await
            .expect("read manifest")
            .expect("manifest bytes");
        assert_eq!(
            std::str::from_utf8(&manifest).expect("manifest utf8"),
            "{\"override\":true}\n",
            "existing delta file should not be overwritten by hydration"
        );
        let delta_only = agent
            .fs
            .read_file("/aiim/custom.txt")
            .await
            .expect("read delta file")
            .expect("delta file bytes");
        assert_eq!(
            std::str::from_utf8(&delta_only).expect("delta utf8"),
            "delta-only"
        );

        drop(agent);
        cleanup_agent(&id);
    }
}

/// Print schema version mismatch error and exit.
fn exit_schema_version_mismatch(found: &str, expected: &str, id_or_path: &str) -> ! {
    eprintln!("Error: Filesystem `{}` requires migration", id_or_path);
    eprintln!();
    eprintln!(
        "Found schema version {}, but this version of agentfs requires {}.",
        found, expected
    );
    eprintln!();
    eprintln!("To upgrade, run:");
    eprintln!();
    eprintln!("    agentfs migrate {}", id_or_path);
    eprintln!();
    std::process::exit(1);
}
