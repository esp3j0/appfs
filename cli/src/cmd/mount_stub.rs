use anyhow::Result;
use std::{io::Write, path::PathBuf};

pub use crate::opts::MountBackend;

/// Arguments for the mount command.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
    /// Load AppFS app routing from the persisted managed registry.
    pub managed_appfs: bool,
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

/// List all currently mounted agentfs filesystems
pub fn list_mounts<W: Write>(out: &mut W) {
    let _ = writeln!(out, "Mount listing is only available on Unix.");
}

/// Mount the agent filesystem.
pub fn mount(_args: MountArgs) -> Result<()> {
    anyhow::bail!("Mounting is only available on Unix (Linux or macOS)")
}

/// Prune unused agentfs mount points.
pub fn prune_mounts(_force: bool) -> Result<()> {
    anyhow::bail!("Mount pruning is only available on Unix")
}
