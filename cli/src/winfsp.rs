//! WinFsp filesystem adapter for AgentFS.
//!
//! This adapter implements the WinFsp FileSystemContext trait by wrapping
//! the agentfs_sdk::FileSystem trait. It allows mounting AgentFS on Windows.
#![allow(clippy::await_holding_lock)]
#![allow(clippy::manual_is_multiple_of)]

use agentfs_sdk::error::Error as SdkError;
use agentfs_sdk::filesystem::TimeChange;
use agentfs_sdk::{BoxedFile, FileSystem, Stats};
use anyhow::Result;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    ffi::c_void,
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::runtime::Handle;
use tracing;
use winfsp::constants::FspCleanupFlags;
use winfsp::filesystem::{
    DirInfo, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo, WideNameInfo,
};
use winfsp::{FspError, U16CStr};

// Windows file attribute constants
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x00000010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x00000020;
const FILE_ATTRIBUTE_READONLY: u32 = 0x00000001;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x00000400;

// Windows create options flags
const FILE_DIRECTORY_FILE: u32 = 0x00000001;
const FILE_DELETE_ON_CLOSE: u32 = 0x00001000;
const FILE_READ_DATA: u32 = 0x00000001;
const FILE_WRITE_DATA: u32 = 0x00000002;
const FILE_APPEND_DATA: u32 = 0x00000004;
const GENERIC_READ_ACCESS: u32 = 0x80000000;
const GENERIC_WRITE_ACCESS: u32 = 0x40000000;
const DELETE_ACCESS: u32 = 0x00010000;
const OPEN_RDONLY: i32 = 0x0000;
const OPEN_WRONLY: i32 = 0x0001;
const OPEN_RDWR: i32 = 0x0002;
const OPEN_NO_READ_HINT: i32 = 0x2000_0000;

// Reparse tag/constants for symbolic links
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000000C;
const SYMLINK_FLAG_RELATIVE: u32 = 0x00000001;

// NTSTATUS error codes (these are negative values when interpreted as i32)
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;
const STATUS_INVALID_PARAMETER: i32 = 0xC000_000Du32 as i32;
const STATUS_DISK_FULL: i32 = 0xC000_007Fu32 as i32;
const STATUS_OBJECT_NAME_INVALID: i32 = 0xC000_0033u32 as i32;
const STATUS_NOT_A_REPARSE_POINT: i32 = 0xC000_0275u32 as i32;
const STATUS_IO_REPARSE_DATA_INVALID: i32 = 0xC000_0278u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;

/// Convert an SDK error to a WinFsp error code.
fn error_to_ntstatus(e: &SdkError) -> i32 {
    match e {
        SdkError::Fs(fs_err) => match fs_err {
            agentfs_sdk::filesystem::FsError::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            agentfs_sdk::filesystem::FsError::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            agentfs_sdk::filesystem::FsError::NotADirectory => STATUS_NOT_A_DIRECTORY,
            agentfs_sdk::filesystem::FsError::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
            agentfs_sdk::filesystem::FsError::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
            agentfs_sdk::filesystem::FsError::InvalidPath => STATUS_OBJECT_NAME_INVALID,
            agentfs_sdk::filesystem::FsError::RootOperation => STATUS_ACCESS_DENIED,
            agentfs_sdk::filesystem::FsError::SymlinkLoop => STATUS_INVALID_PARAMETER,
            agentfs_sdk::filesystem::FsError::InvalidRename => STATUS_INVALID_PARAMETER,
            agentfs_sdk::filesystem::FsError::NameTooLong => STATUS_OBJECT_NAME_INVALID,
            agentfs_sdk::filesystem::FsError::NotASymlink => STATUS_INVALID_PARAMETER,
        },
        SdkError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
            std::io::ErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            std::io::ErrorKind::InvalidInput => STATUS_INVALID_PARAMETER,
            std::io::ErrorKind::StorageFull => STATUS_DISK_FULL,
            _ => STATUS_INVALID_PARAMETER,
        },
        _ => STATUS_INVALID_PARAMETER,
    }
}

/// Resolve file-size behavior for WinFsp `set_file_size`.
///
/// - `set_allocation_size = false`: request is logical EOF change (always apply).
/// - `set_allocation_size = true`: request is allocation-size hint.
///   Since AgentFS has no separate allocation-size primitive, we only apply
///   truncation when allocation shrinks below current logical size.
fn resolve_truncate_size(
    current_size: u64,
    requested_size: u64,
    set_allocation_size: bool,
) -> Option<u64> {
    if !set_allocation_size {
        return Some(requested_size);
    }

    if requested_size < current_size {
        Some(requested_size)
    } else {
        None
    }
}

/// Convert an anyhow error to a WinFsp error code.
fn anyhow_to_ntstatus(e: &anyhow::Error) -> i32 {
    // First try to downcast to SdkError
    if let Some(sdk_err) = e.downcast_ref::<SdkError>() {
        return error_to_ntstatus(sdk_err);
    }
    // Try to downcast to std::io::Error
    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
        match io_err.kind() {
            std::io::ErrorKind::NotFound => return STATUS_OBJECT_NAME_NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => return STATUS_ACCESS_DENIED,
            std::io::ErrorKind::AlreadyExists => return STATUS_OBJECT_NAME_COLLISION,
            std::io::ErrorKind::InvalidInput => return STATUS_INVALID_PARAMETER,
            std::io::ErrorKind::StorageFull => return STATUS_DISK_FULL,
            _ => {}
        }
    }
    STATUS_INVALID_PARAMETER
}

/// Convert Unix mode to Windows file attributes
fn mode_to_attributes(mode: u32) -> u32 {
    let mut attrs = FILE_ATTRIBUTE_NORMAL;

    // Check file type
    let file_type = mode & 0o170000;
    if file_type == 0o040000 {
        attrs = FILE_ATTRIBUTE_DIRECTORY;
    } else if file_type == 0o120000 {
        attrs |= FILE_ATTRIBUTE_REPARSE_POINT;
    }

    // Check if file is read-only (no write permission for owner)
    if (mode & 0o200) == 0 {
        attrs |= FILE_ATTRIBUTE_READONLY;
    }

    attrs
}

/// Convert Stats to FileInfo for WinFsp.
fn fill_file_info(stats: &Stats, file_info: &mut FileInfo) {
    file_info.file_attributes = mode_to_attributes(stats.mode);
    file_info.reparse_tag = if stats.is_symlink() {
        IO_REPARSE_TAG_SYMLINK
    } else {
        0
    };
    file_info.allocation_size = (((stats.size + 4095) / 4096) * 4096) as u64;
    file_info.file_size = stats.size as u64;
    // Convert Unix timestamps to Windows FILETIME
    const UNIX_EPOCH_DIFF: i64 = 11644473600;
    file_info.creation_time = ((stats.ctime + UNIX_EPOCH_DIFF) * 10_000_000) as u64;
    file_info.last_access_time = ((stats.atime + UNIX_EPOCH_DIFF) * 10_000_000) as u64;
    file_info.last_write_time = ((stats.mtime + UNIX_EPOCH_DIFF) * 10_000_000) as u64;
    file_info.change_time = file_info.last_write_time;
    file_info.index_number = stats.ino as u64;
    file_info.hard_links = 1;
    file_info.ea_size = 0;
}

fn volume_capacity(bytes_used: u64) -> (u64, u64) {
    const MIN_TOTAL_SIZE: u64 = 1024 * 1024 * 1024;
    const HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

    let total_size = MIN_TOTAL_SIZE.max(bytes_used.saturating_add(HEADROOM_BYTES));
    let free_size = total_size.saturating_sub(bytes_used);
    (total_size, free_size)
}

fn granted_access_to_open_flags(granted_access: u32) -> i32 {
    let wants_read =
        granted_access == 0 || (granted_access & (FILE_READ_DATA | GENERIC_READ_ACCESS)) != 0;
    let wants_write =
        (granted_access & (FILE_WRITE_DATA | FILE_APPEND_DATA | GENERIC_WRITE_ACCESS)) != 0;
    let wants_delete = (granted_access & DELETE_ACCESS) != 0;

    let mut flags = if wants_write {
        if wants_read {
            OPEN_RDWR
        } else {
            OPEN_WRONLY
        }
    } else {
        OPEN_RDONLY
    };

    if !wants_read && (wants_write || wants_delete || granted_access != 0) {
        flags |= OPEN_NO_READ_HINT;
    }

    flags
}

/// Tracks an open file or directory handle
struct OpenFile {
    /// The file handle (None for directories)
    file: Option<BoxedFile>,
    /// The inode number of the opened file or directory
    ino: i64,
    /// Whether this is a directory
    is_dir: bool,
    /// Whether this is a symbolic link (reparse point)
    is_symlink: bool,
    /// Pending delete flag - file should be deleted on close
    delete_on_close: std::sync::atomic::AtomicBool,
    /// True once deletion has already been executed successfully.
    deleted: std::sync::atomic::AtomicBool,
    /// Path to the file (for deletion on close)
    path: String,
}

/// WinFsp filesystem adapter wrapping an AgentFS FileSystem.
pub struct AgentFSWinFsp {
    fs: Arc<Mutex<dyn FileSystem + Send>>,
    handle: Handle,
    open_files: Mutex<HashMap<u64, OpenFile>>,
    next_fh: AtomicU64,
}

impl AgentFSWinFsp {
    /// Create a new WinFsp filesystem adapter wrapping a FileSystem instance.
    pub fn new(fs: Arc<Mutex<dyn FileSystem + Send>>, handle: Handle) -> Self {
        Self {
            fs,
            handle,
            open_files: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
        }
    }

    /// Execute an async future in a WinFsp sync callback safely.
    /// Uses block_in_place to allow blocking in an async context,
    /// then handle.block_on to run the future on the existing runtime.
    fn block_on<F: Future>(&self, f: F) -> F::Output {
        tokio::task::block_in_place(|| self.handle.block_on(f))
    }

    fn alloc_fh(&self) -> u64 {
        self.next_fh.fetch_add(1, Ordering::SeqCst)
    }

    fn win_path_to_unix(path: &U16CStr) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    /// Parse a path into (parent_ino, name) for operations that need a parent directory.
    /// For multi-level paths, this will walk the path to find the parent directory.
    fn parse_path(&self, path: &str) -> Result<(i64, String)> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok((1, String::new()));
        }

        // Split path into components
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return Ok((1, String::new()));
        }

        // The last component is the name, everything before is the path to parent
        let name = components.last().unwrap().to_string();
        if components.len() == 1 {
            // Direct child of root
            return Ok((1, name));
        }

        // Walk the path to find the parent directory
        let mut current_ino: i64 = 1;
        for component in &components[..components.len() - 1] {
            let fs = self.fs.clone();
            let component_owned = component.to_string();
            let result =
                self.block_on(async move { fs.lock().lookup(current_ino, &component_owned).await });

            match result {
                Ok(Some(stats)) => {
                    if stats.is_directory() {
                        current_ino = stats.ino;
                    } else {
                        return Err(anyhow::anyhow!("Not a directory"));
                    }
                }
                Ok(None) => return Err(anyhow::anyhow!("Path not found")),
                Err(e) => return Err(e.into()),
            }
        }

        Ok((current_ino, name))
    }

    /// Look up a path and return its stats. Walks the entire path.
    fn path_lookup(&self, path: &str) -> Result<Option<Stats>> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            let fs = self.fs.clone();
            return Ok(self.block_on(async move { fs.lock().getattr(1).await })?);
        }

        let (parent_ino, name) = self.parse_path(path)?;
        let fs = self.fs.clone();
        Ok(self.block_on(async move { fs.lock().lookup(parent_ino, &name).await })?)
    }

    /// Delete a file or directory by path.
    fn delete_path(&self, path: &str, is_dir: bool) -> Result<()> {
        let (parent_ino, name) = self.parse_path(path)?;
        if name.is_empty() {
            return Err(anyhow::anyhow!("Cannot delete root directory"));
        }

        let fs = self.fs.clone();
        self.block_on(async move {
            let fs_guard = fs.lock();
            if is_dir {
                fs_guard.rmdir(parent_ino, &name).await
            } else {
                fs_guard.unlink(parent_ino, &name).await
            }
        })?;
        Ok(())
    }

    fn readlink_by_ino(&self, ino: i64) -> Result<String> {
        let fs = self.fs.clone();
        let target = self.block_on(async move { fs.lock().readlink(ino).await })?;
        target.ok_or_else(|| anyhow::anyhow!("Not a symlink"))
    }

    fn is_relative_symlink_target(target: &str) -> bool {
        let bytes = target.as_bytes();
        if target.starts_with(r"\??\") || target.starts_with(r"\\") {
            return false;
        }
        if bytes.len() >= 2 && bytes[1] == b':' {
            return false;
        }
        if target.starts_with('\\') || target.starts_with('/') {
            return false;
        }
        true
    }

    fn to_substitute_symlink_target(print_target: &str) -> String {
        if print_target.starts_with(r"\??\") {
            return print_target.to_string();
        }
        if let Some(rest) = print_target.strip_prefix(r"\\") {
            return format!(r"\??\UNC\{}", rest);
        }
        format!(r"\??\{}", print_target)
    }

    fn build_symlink_reparse_buffer(target: &str) -> Result<Vec<u8>> {
        let print_name = target.replace('/', "\\");
        let is_relative = Self::is_relative_symlink_target(&print_name);
        let substitute_name = if is_relative {
            print_name.clone()
        } else {
            Self::to_substitute_symlink_target(&print_name)
        };
        let flags = if is_relative {
            SYMLINK_FLAG_RELATIVE
        } else {
            0
        };

        let substitute_u16: Vec<u16> = substitute_name.encode_utf16().collect();
        let print_u16: Vec<u16> = print_name.encode_utf16().collect();

        let substitute_len = substitute_u16.len() * 2;
        let print_len = print_u16.len() * 2;
        let data_len = 12usize
            .checked_add(substitute_len)
            .and_then(|v| v.checked_add(print_len))
            .ok_or_else(|| anyhow::anyhow!("Reparse buffer length overflow"))?;

        if substitute_len > u16::MAX as usize
            || print_len > u16::MAX as usize
            || data_len > u16::MAX as usize
        {
            return Err(anyhow::anyhow!("Symlink target is too long"));
        }

        let total_len = 8 + data_len;
        let mut out = vec![0u8; total_len];

        out[0..4].copy_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
        out[4..6].copy_from_slice(&(data_len as u16).to_le_bytes());
        out[6..8].copy_from_slice(&0u16.to_le_bytes()); // reserved

        out[8..10].copy_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        out[10..12].copy_from_slice(&(substitute_len as u16).to_le_bytes());
        out[12..14].copy_from_slice(&(substitute_len as u16).to_le_bytes()); // PrintNameOffset
        out[14..16].copy_from_slice(&(print_len as u16).to_le_bytes());
        out[16..20].copy_from_slice(&flags.to_le_bytes());

        let mut cursor = 20usize;
        for u in substitute_u16 {
            out[cursor..cursor + 2].copy_from_slice(&u.to_le_bytes());
            cursor += 2;
        }
        for u in print_u16 {
            out[cursor..cursor + 2].copy_from_slice(&u.to_le_bytes());
            cursor += 2;
        }

        Ok(out)
    }

    fn parse_symlink_reparse_buffer(buffer: &[u8]) -> Result<String> {
        if buffer.len() < 20 {
            return Err(anyhow::anyhow!("Reparse buffer too small"));
        }

        let tag = u32::from_le_bytes(buffer[0..4].try_into().unwrap());
        if tag != IO_REPARSE_TAG_SYMLINK {
            return Err(anyhow::anyhow!("Unsupported reparse tag: 0x{tag:08x}"));
        }

        let data_len = u16::from_le_bytes(buffer[4..6].try_into().unwrap()) as usize;
        let expected_len = 8usize
            .checked_add(data_len)
            .ok_or_else(|| anyhow::anyhow!("Invalid reparse length"))?;
        if buffer.len() < expected_len || expected_len < 20 {
            return Err(anyhow::anyhow!("Invalid reparse payload length"));
        }

        let substitute_off = u16::from_le_bytes(buffer[8..10].try_into().unwrap()) as usize;
        let substitute_len = u16::from_le_bytes(buffer[10..12].try_into().unwrap()) as usize;
        let print_off = u16::from_le_bytes(buffer[12..14].try_into().unwrap()) as usize;
        let print_len = u16::from_le_bytes(buffer[14..16].try_into().unwrap()) as usize;
        let flags = u32::from_le_bytes(buffer[16..20].try_into().unwrap());

        let path_start = 20usize;
        let data_end = expected_len;

        let read_utf16 = |off: usize, len: usize| -> Result<String> {
            if len == 0 {
                return Ok(String::new());
            }
            if len % 2 != 0 {
                return Err(anyhow::anyhow!("Invalid UTF-16 byte length"));
            }
            let start = path_start
                .checked_add(off)
                .ok_or_else(|| anyhow::anyhow!("Reparse offset overflow"))?;
            let end = start
                .checked_add(len)
                .ok_or_else(|| anyhow::anyhow!("Reparse length overflow"))?;
            if end > data_end {
                return Err(anyhow::anyhow!("Reparse name out of bounds"));
            }

            let mut utf16 = Vec::with_capacity(len / 2);
            let mut idx = start;
            while idx < end {
                utf16.push(u16::from_le_bytes([buffer[idx], buffer[idx + 1]]));
                idx += 2;
            }
            Ok(String::from_utf16(&utf16)?)
        };

        let substitute_name = read_utf16(substitute_off, substitute_len)?;
        let print_name = read_utf16(print_off, print_len)?;

        let mut target = if !print_name.is_empty() {
            print_name
        } else {
            substitute_name
        };

        if (flags & SYMLINK_FLAG_RELATIVE) == 0 {
            if let Some(rest) = target.strip_prefix(r"\??\UNC\") {
                target = format!(r"\\{}", rest);
            } else if let Some(rest) = target.strip_prefix(r"\??\") {
                target = rest.to_string();
            }
        }

        Ok(target)
    }

    fn write_symlink_reparse_to_buffer(&self, ino: i64, buffer: &mut [u8]) -> winfsp::Result<u64> {
        let target = self
            .readlink_by_ino(ino)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;
        let encoded = Self::build_symlink_reparse_buffer(&target)
            .map_err(|_| FspError::NTSTATUS(STATUS_IO_REPARSE_DATA_INVALID))?;

        if buffer.len() < encoded.len() {
            return Err(FspError::NTSTATUS(STATUS_BUFFER_TOO_SMALL));
        }
        buffer[..encoded.len()].copy_from_slice(&encoded);
        Ok(encoded.len() as u64)
    }
}

/// File context for WinFsp - represents an open file handle.
pub struct FileContext {
    fh: u64,
}

impl FileSystemContext for AgentFSWinFsp {
    type FileContext = FileContext;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let path = Self::win_path_to_unix(file_name);

        tracing::debug!("WinFsp::get_security_by_name: {}", path);

        match self.path_lookup(&path) {
            Ok(Some(stats)) => {
                Ok(FileSecurity {
                    // Important: reparse=true is for paths that contain reparse points
                    // in intermediate components. For a successfully looked-up final
                    // component (including a symlink itself), return normal attributes.
                    reparse: false,
                    attributes: mode_to_attributes(stats.mode),
                    sz_security_descriptor: 0,
                })
            }
            Ok(None) => Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
            Err(e) => {
                // Fallback to resolver for paths that may contain intermediate reparse points
                // (e.g. dir symlink component). Avoid resolver-first to keep create-path
                // lookups (where final component does not exist) stable.
                if let Some(security) = reparse_point_resolver(file_name) {
                    Ok(security)
                } else {
                    Err(FspError::NTSTATUS(anyhow_to_ntstatus(&e)))
                }
            }
        }
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let path = Self::win_path_to_unix(file_name);
        let delete_on_close = (create_options & FILE_DELETE_ON_CLOSE) != 0;

        tracing::debug!(
            "WinFsp::open: path={} create_options=0x{:x} granted_access=0x{:x} delete_on_close={}",
            path,
            create_options,
            granted_access,
            delete_on_close
        );

        match self.path_lookup(&path) {
            Ok(Some(stats)) => {
                fill_file_info(&stats, file_info.as_mut());

                let fh = self.alloc_fh();
                let is_dir = stats.is_directory();
                let path_owned = path.clone();

                if is_dir {
                    // For directories, we don't need a file handle, just track the inode
                    self.open_files.lock().insert(
                        fh,
                        OpenFile {
                            file: None,
                            ino: stats.ino,
                            is_dir: true,
                            is_symlink: false,
                            delete_on_close: std::sync::atomic::AtomicBool::new(delete_on_close),
                            deleted: std::sync::atomic::AtomicBool::new(false),
                            path: path_owned,
                        },
                    );
                    Ok(FileContext { fh })
                } else if stats.is_symlink() {
                    self.open_files.lock().insert(
                        fh,
                        OpenFile {
                            file: None,
                            ino: stats.ino,
                            is_dir: false,
                            is_symlink: true,
                            delete_on_close: std::sync::atomic::AtomicBool::new(delete_on_close),
                            deleted: std::sync::atomic::AtomicBool::new(false),
                            path: path_owned,
                        },
                    );
                    Ok(FileContext { fh })
                } else {
                    // For files, open the file handle
                    let fs = self.fs.clone();
                    let ino = stats.ino;
                    let open_flags = granted_access_to_open_flags(granted_access);
                    let file = self.block_on(async move { fs.lock().open(ino, open_flags).await });

                    match file {
                        Ok(file) => {
                            self.open_files.lock().insert(
                                fh,
                                OpenFile {
                                    file: Some(file),
                                    ino: stats.ino,
                                    is_dir: false,
                                    is_symlink: false,
                                    delete_on_close: std::sync::atomic::AtomicBool::new(
                                        delete_on_close,
                                    ),
                                    deleted: std::sync::atomic::AtomicBool::new(false),
                                    path: path_owned,
                                },
                            );
                            Ok(FileContext { fh })
                        }
                        Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                    }
                }
            }
            Ok(None) => Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
            Err(e) => Err(FspError::NTSTATUS(anyhow_to_ntstatus(&e))),
        }
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        extra_buffer: Option<&[u8]>,
        extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let path = Self::win_path_to_unix(file_name);
        let is_dir = (create_options & FILE_DIRECTORY_FILE) != 0;
        let delete_on_close = (create_options & FILE_DELETE_ON_CLOSE) != 0;

        tracing::debug!(
            "WinFsp::create: {} (is_dir={} extra_buffer_is_reparse_point={})",
            path,
            is_dir,
            extra_buffer_is_reparse_point
        );

        // Parse path to get parent_ino and name
        let (parent_ino, name) = self
            .parse_path(&path)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;

        // First, check if the file already exists
        let existing = {
            let fs = self.fs.clone();
            let name_clone = name.clone();
            self.block_on(async move { fs.lock().lookup(parent_ino, &name_clone).await })
        };

        match existing {
            Ok(Some(stats)) => {
                // File already exists - open it directly
                // WinFsp will call overwrite() if truncation is needed
                tracing::debug!(
                    "WinFsp::create: file exists, opening {} (ino={})",
                    path,
                    stats.ino
                );
                fill_file_info(&stats, file_info.as_mut());

                let fh = self.alloc_fh();
                let path_owned = path.clone();

                if stats.is_directory() {
                    self.open_files.lock().insert(
                        fh,
                        OpenFile {
                            file: None,
                            ino: stats.ino,
                            is_dir: true,
                            is_symlink: false,
                            delete_on_close: std::sync::atomic::AtomicBool::new(delete_on_close),
                            deleted: std::sync::atomic::AtomicBool::new(false),
                            path: path_owned,
                        },
                    );
                    Ok(FileContext { fh })
                } else if stats.is_symlink() {
                    self.open_files.lock().insert(
                        fh,
                        OpenFile {
                            file: None,
                            ino: stats.ino,
                            is_dir: false,
                            is_symlink: true,
                            delete_on_close: std::sync::atomic::AtomicBool::new(delete_on_close),
                            deleted: std::sync::atomic::AtomicBool::new(false),
                            path: path_owned,
                        },
                    );
                    Ok(FileContext { fh })
                } else {
                    let fs = self.fs.clone();
                    let ino = stats.ino;
                    let open_flags = granted_access_to_open_flags(granted_access);
                    let file = self.block_on(async move { fs.lock().open(ino, open_flags).await });

                    match file {
                        Ok(file) => {
                            self.open_files.lock().insert(
                                fh,
                                OpenFile {
                                    file: Some(file),
                                    ino: stats.ino,
                                    is_dir: false,
                                    is_symlink: false,
                                    delete_on_close: std::sync::atomic::AtomicBool::new(
                                        delete_on_close,
                                    ),
                                    deleted: std::sync::atomic::AtomicBool::new(false),
                                    path: path_owned,
                                },
                            );
                            Ok(FileContext { fh })
                        }
                        Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                    }
                }
            }
            Ok(None) => {
                // File does not exist - create it
                tracing::debug!("WinFsp::create: file does not exist, creating {}", path);

                let fs = self.fs.clone();
                let name_owned = name.clone();

                // Use uid=0, gid=0 for now (root user)
                // TODO: Get actual user context from Windows
                let uid = 0u32;
                let gid = 0u32;

                let result = if extra_buffer_is_reparse_point {
                    let reparse_buffer =
                        extra_buffer.ok_or(FspError::NTSTATUS(STATUS_IO_REPARSE_DATA_INVALID))?;
                    let target = Self::parse_symlink_reparse_buffer(reparse_buffer)
                        .map_err(|_| FspError::NTSTATUS(STATUS_IO_REPARSE_DATA_INVALID))?;
                    let target_owned = target.clone();
                    self.block_on(async move {
                        fs.lock()
                            .symlink(parent_ino, &name_owned, &target_owned, uid, gid)
                            .await
                    })
                } else if is_dir {
                    // Create directory with mode 0755 (rwxr-xr-x)
                    self.block_on(async move {
                        fs.lock()
                            .mkdir(parent_ino, &name_owned, 0o755, uid, gid)
                            .await
                    })
                } else {
                    // Create file with mode 0644 (rw-r--r--)
                    self.block_on(async move {
                        fs.lock()
                            .mknod(parent_ino, &name_owned, 0o100644, 0, uid, gid)
                            .await
                    })
                };

                match result {
                    Ok(stats) => {
                        fill_file_info(&stats, file_info.as_mut());

                        let fh = self.alloc_fh();
                        let path_owned = path.clone();

                        if stats.is_directory() {
                            // For directories, we don't need a file handle
                            self.open_files.lock().insert(
                                fh,
                                OpenFile {
                                    file: None,
                                    ino: stats.ino,
                                    is_dir: true,
                                    is_symlink: false,
                                    delete_on_close: std::sync::atomic::AtomicBool::new(
                                        delete_on_close,
                                    ),
                                    deleted: std::sync::atomic::AtomicBool::new(false),
                                    path: path_owned,
                                },
                            );
                            Ok(FileContext { fh })
                        } else if stats.is_symlink() {
                            self.open_files.lock().insert(
                                fh,
                                OpenFile {
                                    file: None,
                                    ino: stats.ino,
                                    is_dir: false,
                                    is_symlink: true,
                                    delete_on_close: std::sync::atomic::AtomicBool::new(
                                        delete_on_close,
                                    ),
                                    deleted: std::sync::atomic::AtomicBool::new(false),
                                    path: path_owned,
                                },
                            );
                            Ok(FileContext { fh })
                        } else {
                            // For files, open the newly created file
                            let fs = self.fs.clone();
                            let ino = stats.ino;
                            let open_flags = granted_access_to_open_flags(granted_access);
                            let file =
                                self.block_on(async move { fs.lock().open(ino, open_flags).await });

                            match file {
                                Ok(file) => {
                                    self.open_files.lock().insert(
                                        fh,
                                        OpenFile {
                                            file: Some(file),
                                            ino: stats.ino,
                                            is_dir: false,
                                            is_symlink: false,
                                            delete_on_close: std::sync::atomic::AtomicBool::new(
                                                delete_on_close,
                                            ),
                                            deleted: std::sync::atomic::AtomicBool::new(false),
                                            path: path_owned,
                                        },
                                    );
                                    Ok(FileContext { fh })
                                }
                                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                            }
                        }
                    }
                    Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                }
            }
            Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
        }
    }

    fn close(&self, context: Self::FileContext) {
        // WinFsp performs deletions during cleanup(FspCleanupDelete), not close().
        self.open_files.lock().remove(&context.fh);
    }

    fn cleanup(&self, context: &Self::FileContext, file_name: Option<&U16CStr>, flags: u32) {
        let (should_delete, is_dir, fallback_path) = {
            let open_files = self.open_files.lock();
            let Some(open_file) = open_files.get(&context.fh) else {
                return;
            };

            let delete_flag = FspCleanupFlags::FspCleanupDelete.is_flagged(flags);
            let pending_delete = open_file.delete_on_close.load(Ordering::SeqCst);
            let already_deleted = open_file.deleted.load(Ordering::SeqCst);
            (
                !already_deleted && (delete_flag || pending_delete),
                open_file.is_dir,
                open_file.path.clone(),
            )
        };

        tracing::debug!(
            "WinFsp::cleanup: fh={} flags=0x{:x} should_delete={} is_dir={} path={}",
            context.fh,
            flags,
            should_delete,
            is_dir,
            fallback_path
        );

        if !should_delete {
            return;
        }

        let path = file_name
            .map(Self::win_path_to_unix)
            .filter(|p| !p.is_empty())
            .unwrap_or(fallback_path);

        if path.trim_matches('/').is_empty() {
            tracing::warn!("Refusing to delete root during cleanup");
            return;
        }

        if let Err(e) = self.delete_path(&path, is_dir) {
            // Windows cleanup cannot report failure; log for diagnosis.
            tracing::warn!("Failed to delete {} during cleanup: {}", path, e);
        }
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            let ino = open_file.ino;
            drop(open_files);

            let fs = self.fs.clone();
            let stats = self.block_on(async move { fs.lock().getattr(ino).await });

            match stats {
                Ok(Some(stats)) => {
                    fill_file_info(&stats, file_info);
                    Ok(())
                }
                Ok(None) => Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn get_security(
        &self,
        context: &Self::FileContext,
        _security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        tracing::debug!("WinFsp::get_security: fh={}", context.fh);
        Ok(0)
    }

    fn set_security(
        &self,
        context: &Self::FileContext,
        security_information: u32,
        _modification_descriptor: winfsp::filesystem::ModificationDescriptor,
    ) -> winfsp::Result<()> {
        tracing::debug!(
            "WinFsp::set_security: fh={} security_information=0x{:x}",
            context.fh,
            security_information
        );
        // AgentFS currently does not persist Windows ACLs; accept and ignore.
        Ok(())
    }

    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            let ino = open_file.ino;
            tracing::debug!(
                "WinFsp::set_basic_info: fh={} ino={} last_access_time={} last_write_time={}",
                context.fh,
                ino,
                last_access_time,
                last_write_time
            );
            drop(open_files);

            let atime = if last_access_time == 0 {
                TimeChange::Omit
            } else {
                TimeChange::Set((last_access_time as i64 / 10_000_000) - 11644473600, 0)
            };
            let mtime = if last_write_time == 0 {
                TimeChange::Omit
            } else {
                TimeChange::Set((last_write_time as i64 / 10_000_000) - 11644473600, 0)
            };

            let fs = self.fs.clone();
            let result = self.block_on(async move { fs.lock().utimens(ino, atime, mtime).await });

            match result {
                Ok(()) => {
                    let fs = self.fs.clone();
                    let stats = self.block_on(async move { fs.lock().getattr(ino).await });
                    match stats {
                        Ok(Some(stats)) => {
                            fill_file_info(&stats, file_info);
                            Ok(())
                        }
                        _ => Ok(()),
                    }
                }
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        let path = Self::win_path_to_unix(file_name);
        let (ino, is_dir) = {
            let open_files = self.open_files.lock();
            let Some(open_file) = open_files.get(&context.fh) else {
                return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
            };
            (open_file.ino, open_file.is_dir)
        };

        tracing::debug!(
            "WinFsp::set_delete: fh={} path={} delete_file={} is_dir={} ino={}",
            context.fh,
            path,
            delete_file,
            is_dir,
            ino
        );

        // Validate directory emptiness during SetDelete so Remove-Item can fail correctly.
        if delete_file && is_dir {
            let fs = self.fs.clone();
            let entries = self.block_on(async move { fs.lock().readdir_plus(ino).await });

            match entries {
                Ok(Some(entries)) => {
                    if !entries.is_empty() {
                        return Err(FspError::NTSTATUS(STATUS_DIRECTORY_NOT_EMPTY));
                    }
                }
                Ok(None) => return Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
                Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        }

        if delete_file {
            if let Err(e) = self.delete_path(&path, is_dir) {
                return Err(FspError::NTSTATUS(anyhow_to_ntstatus(&e)));
            }
        }

        let open_files = self.open_files.lock();
        let Some(open_file) = open_files.get(&context.fh) else {
            return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
        };
        open_file.delete_on_close.store(false, Ordering::SeqCst);
        open_file.deleted.store(delete_file, Ordering::SeqCst);
        if !delete_file {
            open_file.deleted.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    fn rename(
        &self,
        _context: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        _replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        let old_path = Self::win_path_to_unix(file_name);
        let new_path = Self::win_path_to_unix(new_file_name);
        tracing::debug!("WinFsp::rename: {} -> {}", old_path, new_path);
        let (old_parent, old_name) = self
            .parse_path(&old_path)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;
        let (new_parent, new_name) = self
            .parse_path(&new_path)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;

        let fs = self.fs.clone();
        let result = self.block_on(async move {
            fs.lock()
                .rename(old_parent, &old_name, new_parent, &new_name)
                .await
        });

        match result {
            Ok(()) => Ok(()),
            Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
        }
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: winfsp::filesystem::DirMarker<'_>,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        let marker_str = marker
            .inner_as_cstr()
            .map(|m| m.to_string_lossy())
            .unwrap_or_else(|| "<none>".to_string());
        tracing::debug!(
            "WinFsp::read_directory: fh={} marker={} is_none={} is_current={} is_parent={}",
            context.fh,
            marker_str,
            marker.is_none(),
            marker.is_current(),
            marker.is_parent()
        );
        // Get the directory inode/path from the open file handle
        let (dir_ino, dir_path) = {
            let open_files = self.open_files.lock();
            match open_files.get(&context.fh) {
                Some(open_file) => (open_file.ino, open_file.path.clone()),
                None => return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER)),
            }
        };

        let fs = self.fs.clone();
        let entries = self.block_on(async move { fs.lock().readdir_plus(dir_ino).await });

        match entries {
            Ok(Some(entries)) => {
                let fs = self.fs.clone();
                let dir_stats = self.block_on(async move { fs.lock().getattr(dir_ino).await });
                let dir_stats = match dir_stats {
                    Ok(Some(stats)) => stats,
                    Ok(None) => return Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
                    Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                };

                let parent_ino = if dir_path == "/" {
                    1
                } else {
                    match self.parse_path(&dir_path) {
                        Ok((pino, _)) => pino,
                        Err(e) => return Err(FspError::NTSTATUS(anyhow_to_ntstatus(&e))),
                    }
                };
                let fs = self.fs.clone();
                let parent_stats =
                    self.block_on(async move { fs.lock().getattr(parent_ino).await });
                let parent_stats = match parent_stats {
                    Ok(Some(stats)) => stats,
                    Ok(None) => dir_stats.clone(),
                    Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                };

                let mut all_entries: Vec<(String, Stats)> = Vec::with_capacity(entries.len() + 2);
                all_entries.push((".".to_string(), dir_stats));
                all_entries.push(("..".to_string(), parent_stats));
                for entry in entries {
                    all_entries.push((entry.name, entry.stats));
                }
                tracing::debug!(
                    "WinFsp::read_directory: fh={} entry_count={} entries={:?}",
                    context.fh,
                    all_entries.len(),
                    all_entries
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>()
                );

                // Determine starting index based on marker.
                // The marker is the filename (U16CStr) of the last entry returned
                // in the previous call. We need to find it and skip past it.
                let start_idx = if let Some(marker_name) = marker.inner_as_cstr() {
                    let marker_str = marker_name.to_string_lossy();
                    let mut idx = 0usize;
                    for (i, (name, _stats)) in all_entries.iter().enumerate() {
                        if name == &marker_str {
                            idx = i + 1;
                            break;
                        }
                    }
                    idx
                } else {
                    0
                };
                let mut cursor = 0u32;

                for (name, stats) in all_entries.iter().skip(start_idx) {
                    let mut dir_info: DirInfo<255> = DirInfo::default();
                    fill_file_info(stats, dir_info.file_info_mut());

                    // Set the name using WideNameInfo trait
                    if dir_info.set_name(name).is_ok() {
                        // Use the proper WinFsp API to append to buffer
                        // This handles variable-length entries correctly
                        if !dir_info.append_to_buffer(buffer, &mut cursor) {
                            // Buffer is full, stop adding more entries
                            break;
                        }
                    }
                }

                // Finalize the buffer
                DirInfo::<255>::finalize_buffer(buffer, &mut cursor);

                Ok(cursor)
            }
            Ok(None) => Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
            Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
        }
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            let file = open_file.file.clone();
            let buf_len = buffer.len();
            let is_dir = open_file.is_dir;
            let is_symlink = open_file.is_symlink;
            drop(open_files);

            // file is Option<BoxedFile>, need to handle None case
            let file = match file {
                Some(f) => f,
                None => {
                    if is_symlink {
                        return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
                    }
                    if is_dir {
                        return Err(FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
                    }
                    return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
                }
            };

            let result = self.block_on(async move { file.pread(offset, buf_len as u64).await });

            match result {
                Ok(data) => {
                    let len = data.len().min(buffer.len());
                    buffer[..len].copy_from_slice(&data[..len]);
                    Ok(len as u32)
                }
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        _constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            let file = open_file.file.clone();
            let ino = open_file.ino;
            let is_dir = open_file.is_dir;
            let is_symlink = open_file.is_symlink;
            tracing::debug!(
                "WinFsp::write: fh={} ino={} len={} offset={} write_to_eof={}",
                context.fh,
                ino,
                buffer.len(),
                offset,
                write_to_eof
            );
            drop(open_files);

            // file is Option<BoxedFile>, need to handle None case
            let file = match file {
                Some(f) => f,
                None => {
                    if is_symlink {
                        return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
                    }
                    if is_dir {
                        return Err(FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
                    }
                    return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
                }
            };

            let write_offset = if write_to_eof {
                let fs = self.fs.clone();
                match self.block_on(async move { fs.lock().getattr(ino).await }) {
                    Ok(Some(stats)) => stats.size as u64,
                    Ok(None) => return Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
                    Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                }
            } else {
                offset
            };

            let result = self.block_on(async move { file.pwrite(write_offset, buffer).await });

            match result {
                Ok(()) => {
                    let fs = self.fs.clone();
                    let stats = self.block_on(async move { fs.lock().getattr(ino).await });
                    if let Ok(Some(stats)) = stats {
                        fill_file_info(&stats, file_info);
                    }
                    Ok(buffer.len() as u32)
                }
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            tracing::debug!(
                "WinFsp::set_file_size: fh={} ino={} new_size={} set_allocation_size={} is_dir={}",
                context.fh,
                open_file.ino,
                new_size,
                set_allocation_size,
                open_file.is_dir
            );
            // For directories, just return success (no-op)
            if open_file.is_dir || open_file.is_symlink {
                return Ok(());
            }

            let file = open_file.file.clone();
            let ino = open_file.ino;
            drop(open_files);

            let file = match file {
                Some(f) => f,
                None => return Ok(()), // Should not happen, but handle gracefully
            };

            let fs = self.fs.clone();
            let current_stats = match self.block_on(async move { fs.lock().getattr(ino).await }) {
                Ok(Some(stats)) => stats,
                Ok(None) => return Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND)),
                Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            };
            let current_size = current_stats.size.max(0) as u64;

            let Some(target_size) =
                resolve_truncate_size(current_size, new_size, set_allocation_size)
            else {
                // Allocation-size extension should not change logical file length.
                fill_file_info(&current_stats, file_info);
                return Ok(());
            };

            if target_size == current_size {
                fill_file_info(&current_stats, file_info);
                return Ok(());
            }

            let result = self.block_on(async move { file.truncate(target_size).await });

            match result {
                Ok(()) => {
                    let fs = self.fs.clone();
                    if let Ok(Some(stats)) =
                        self.block_on(async move { fs.lock().getattr(ino).await })
                    {
                        fill_file_info(&stats, file_info);
                    }
                    Ok(())
                }
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn overwrite(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        // Handle overwrite: called when creating file with FILE_SUPERSEDE or FILE_OVERWRITE
        // Truncate the file to zero length
        let open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get(&context.fh) {
            tracing::debug!(
                "WinFsp::overwrite: fh={} ino={} is_dir={} is_symlink={}",
                context.fh,
                open_file.ino,
                open_file.is_dir,
                open_file.is_symlink
            );
            if open_file.is_dir || open_file.is_symlink {
                return Ok(());
            }

            let file = open_file.file.clone();
            let ino = open_file.ino;
            drop(open_files);

            let file = match file {
                Some(f) => f,
                None => return Ok(()),
            };

            // Truncate to zero
            let result = self.block_on(async move { file.truncate(0).await });

            match result {
                Ok(()) => {
                    let fs = self.fs.clone();
                    if let Ok(Some(stats)) =
                        self.block_on(async move { fs.lock().getattr(ino).await })
                    {
                        fill_file_info(&stats, file_info);
                    }
                    Ok(())
                }
                Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
            }
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if let Some(context) = context {
            let open_files = self.open_files.lock();
            if let Some(open_file) = open_files.get(&context.fh) {
                let file = open_file.file.clone();
                let ino = open_file.ino;
                drop(open_files);

                // file is Option<BoxedFile>, directories don't need flush
                if let Some(file) = file {
                    let result = self.block_on(async move { file.fsync().await });

                    match result {
                        Ok(()) => {
                            let fs = self.fs.clone();
                            let stats = self.block_on(async move { fs.lock().getattr(ino).await });
                            if let Ok(Some(stats)) = stats {
                                fill_file_info(&stats, file_info);
                            }
                            Ok(())
                        }
                        Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
                    }
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let fs = self.fs.clone();
        let stats = self.block_on(async move { fs.lock().statfs().await });

        match stats {
            Ok(stats) => {
                let (total_size, free_size) = volume_capacity(stats.bytes_used);
                out_volume_info.total_size = total_size;
                out_volume_info.free_size = free_size;
                Ok(())
            }
            Err(e) => Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
        }
    }

    fn get_reparse_point_by_name(
        &self,
        file_name: &U16CStr,
        _is_directory: bool,
        buffer: &mut [u8],
    ) -> winfsp::Result<u64> {
        let path = Self::win_path_to_unix(file_name);
        tracing::debug!("WinFsp::get_reparse_point_by_name path={}", path);
        let stats = self
            .path_lookup(&path)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;
        let Some(stats) = stats else {
            return Err(FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND));
        };
        if !stats.is_symlink() {
            return Err(FspError::NTSTATUS(STATUS_NOT_A_REPARSE_POINT));
        }

        self.write_symlink_reparse_to_buffer(stats.ino, buffer)
    }

    fn get_reparse_point(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        buffer: &mut [u8],
    ) -> winfsp::Result<u64> {
        let ino = {
            let open_files = self.open_files.lock();
            let Some(open_file) = open_files.get(&context.fh) else {
                return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
            };
            if !open_file.is_symlink {
                return Err(FspError::NTSTATUS(STATUS_NOT_A_REPARSE_POINT));
            }
            tracing::debug!(
                "WinFsp::get_reparse_point fh={} ino={} path={}",
                context.fh,
                open_file.ino,
                open_file.path
            );
            open_file.ino
        };

        self.write_symlink_reparse_to_buffer(ino, buffer)
    }

    fn set_reparse_point(
        &self,
        context: &Self::FileContext,
        file_name: &U16CStr,
        buffer: &[u8],
    ) -> winfsp::Result<()> {
        let path = Self::win_path_to_unix(file_name);
        tracing::debug!(
            "WinFsp::set_reparse_point fh={} path={} buffer_len={}",
            context.fh,
            path,
            buffer.len()
        );
        let target = Self::parse_symlink_reparse_buffer(buffer)
            .map_err(|_| FspError::NTSTATUS(STATUS_IO_REPARSE_DATA_INVALID))?;
        tracing::debug!(
            "WinFsp::set_reparse_point parsed target={} path={}",
            target,
            path
        );
        let (parent_ino, name) = self
            .parse_path(&path)
            .map_err(|e| FspError::NTSTATUS(anyhow_to_ntstatus(&e)))?;

        let current_is_dir = {
            let open_files = self.open_files.lock();
            let Some(open_file) = open_files.get(&context.fh) else {
                return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
            };
            open_file.is_dir
        };

        if let Err(e) = self.delete_path(&path, current_is_dir) {
            tracing::warn!(
                "WinFsp::set_reparse_point delete placeholder failed path={} err={}",
                path,
                e
            );
            return Err(FspError::NTSTATUS(anyhow_to_ntstatus(&e)));
        }

        let fs = self.fs.clone();
        let target_owned = target.clone();
        let stats = self.block_on(async move {
            fs.lock()
                .symlink(parent_ino, &name, &target_owned, 0, 0)
                .await
        });
        let stats = match stats {
            Ok(stats) => stats,
            Err(e) => return Err(FspError::NTSTATUS(error_to_ntstatus(&e))),
        };
        tracing::debug!(
            "WinFsp::set_reparse_point created symlink path={} ino={}",
            path,
            stats.ino
        );

        let mut open_files = self.open_files.lock();
        if let Some(open_file) = open_files.get_mut(&context.fh) {
            open_file.file = None;
            open_file.ino = stats.ino;
            open_file.is_dir = false;
            open_file.is_symlink = true;
            open_file.path = path;
            Ok(())
        } else {
            Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER))
        }
    }

    fn delete_reparse_point(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        _buffer: &[u8],
    ) -> winfsp::Result<()> {
        let open_files = self.open_files.lock();
        let Some(open_file) = open_files.get(&context.fh) else {
            return Err(FspError::NTSTATUS(STATUS_INVALID_PARAMETER));
        };
        tracing::debug!(
            "WinFsp::delete_reparse_point fh={} ino={} path={} is_symlink={}",
            context.fh,
            open_file.ino,
            open_file.path,
            open_file.is_symlink
        );
        if !open_file.is_symlink {
            return Err(FspError::NTSTATUS(STATUS_NOT_A_REPARSE_POINT));
        }
        Ok(())
    }
}

/// Mount options for WinFsp.
pub struct MountOpts {
    pub mountpoint: PathBuf,
    pub fsname: String,
}

/// Mount an AgentFS filesystem using WinFsp.
pub fn mount(fs: Arc<Mutex<dyn FileSystem + Send>>, opts: MountOpts) -> Result<()> {
    let mountpoint = opts.mountpoint.clone();
    // Try to get the current runtime handle, or create a new runtime if not in async context
    let handle = match Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            // Not in an async context, need to use a different approach
            // Since this is a sync function without a runtime, we can't really work
            // Let the caller know they need to call this from an async context
            anyhow::bail!("mount() must be called from within a tokio runtime context");
        }
    };
    let adapter = AgentFSWinFsp::new(fs, handle);

    let mut volume_params = winfsp::host::VolumeParams::default();
    volume_params.case_sensitive_search(true);
    volume_params.case_preserved_names(true);
    volume_params.unicode_on_disk(true);
    volume_params.filesystem_name(&opts.fsname);
    volume_params.reparse_points(true);
    volume_params.reparse_points_access_check(true);
    // Allow opening reparse points without strict dir/non-dir create option checks.
    volume_params.no_reparse_points_dir_check(true);
    volume_params.supports_posix_unlink_rename(true);
    // Always post disposition requests to user mode so delete semantics are
    // handled by set_delete/cleanup instead of kernel prechecks.
    volume_params.post_disposition_only_when_necessary(false);

    let mut host = winfsp::host::FileSystemHost::new(volume_params, adapter)?;

    let mountpoint_str = mountpoint.to_string_lossy().to_string();
    tracing::info!("Mounting WinFsp filesystem at {}", mountpoint_str);

    host.mount(mountpoint_str.as_str())?;
    // Note: WinFsp doesn't have a run() method in the traditional sense.
    // The mount() call blocks until the host is dropped
    // The filesystem will continue to operate until the host is dropped
    // For async operation, the mount() returns immediately, so caller needs to
    // keep the host alive if they want to stop the filesystem
    Ok(())
}

/// Unmount a WinFsp filesystem.
pub fn unmount(_mountpoint: &std::path::Path, _lazy: bool) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_truncate_size, volume_capacity};

    #[test]
    fn allocation_growth_does_not_change_logical_size() {
        assert_eq!(resolve_truncate_size(2, 512, true), None);
    }

    #[test]
    fn allocation_shrink_truncates_when_below_logical_size() {
        assert_eq!(resolve_truncate_size(512, 2, true), Some(2));
    }

    #[test]
    fn eof_size_request_always_applies() {
        assert_eq!(resolve_truncate_size(2, 512, false), Some(512));
        assert_eq!(resolve_truncate_size(512, 2, false), Some(2));
    }

    #[test]
    fn volume_capacity_keeps_minimum_headroom() {
        let (total, free) = volume_capacity(1234);
        assert_eq!(total, 1024 * 1024 * 1024);
        assert_eq!(free, total - 1234);
    }

    #[test]
    fn volume_capacity_saturates_when_usage_exceeds_minimum() {
        let bytes_used = 2 * 1024 * 1024 * 1024;
        let (total, free) = volume_capacity(bytes_used);
        assert_eq!(total, bytes_used + 64 * 1024 * 1024);
        assert_eq!(free, 64 * 1024 * 1024);
    }
}
