use super::{
    BoxedFile, DirEntry, File, FileSystem, FilesystemStats, FsError, Stats, TimeChange, S_IFDIR,
    S_IFLNK, S_IFMT, S_IFREG,
};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::fs::{self, File as StdFile, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::os::windows::fs::{FileExt, MetadataExt};

/// Root inode number (matches FUSE convention)
pub const ROOT_INO: i64 = 1;

/// Source file identity derived from a stable normalized path.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SrcId {
    path_id: u64,
}

/// An inode entry caching the path to the file.
struct Inode {
    path: PathBuf,
    src_id: SrcId,
    nlookup: AtomicU64,
}

/// A filesystem backed by a host directory using path-based operations on Windows.
pub struct HostFS {
    root: PathBuf,
    inodes: RwLock<HashMap<i64, Inode>>,
    src_to_ino: RwLock<HashMap<SrcId, i64>>,
    next_ino: AtomicU64,
}

/// An open file handle for HostFS.
pub struct HostFSFile {
    file: StdFile,
    ino: i64,
}

#[async_trait]
impl File for HostFSFile {
    async fn pread(&self, offset: u64, size: u64) -> Result<Vec<u8>> {
        let file = self.file.try_clone()?;
        tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; size as usize];
            let n = file.seek_read(&mut buf, offset)?;
            buf.truncate(n);
            Ok(buf)
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }

    async fn pwrite(&self, offset: u64, data: &[u8]) -> Result<()> {
        let file = self.file.try_clone()?;
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            file.seek_write(&data, offset)?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }

    async fn truncate(&self, size: u64) -> Result<()> {
        let file = self.file.try_clone()?;
        tokio::task::spawn_blocking(move || {
            file.set_len(size)?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }

    async fn fsync(&self) -> Result<()> {
        let file = self.file.try_clone()?;
        tokio::task::spawn_blocking(move || {
            file.sync_all()?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }

    async fn fstat(&self) -> Result<Stats> {
        let file = self.file.try_clone()?;
        let ino = self.ino;
        tokio::task::spawn_blocking(move || {
            let metadata = file.metadata()?;
            Ok(metadata_to_stats(&metadata, ino))
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }
}

fn filetime_to_unix(filetime_100ns: u64) -> (i64, u32) {
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    const HUNDREDS_OF_NS_PER_SEC: u64 = 10_000_000;

    let secs_since_windows_epoch = filetime_100ns / HUNDREDS_OF_NS_PER_SEC;
    let nanos = ((filetime_100ns % HUNDREDS_OF_NS_PER_SEC) * 100) as u32;
    let unix_secs = secs_since_windows_epoch.saturating_sub(EPOCH_DIFF_SECS) as i64;
    (unix_secs, nanos)
}

fn mode_from_metadata(metadata: &Metadata) -> u32 {
    let readonly = metadata.permissions().readonly();
    let file_type = metadata.file_type();
    let perms = if file_type.is_dir() {
        if readonly {
            0o555
        } else {
            0o755
        }
    } else if file_type.is_symlink() {
        0o777
    } else if readonly {
        0o444
    } else {
        0o644
    };

    let kind = if file_type.is_dir() {
        S_IFDIR
    } else if file_type.is_symlink() {
        S_IFLNK
    } else {
        S_IFREG
    };

    kind | perms
}

fn stable_path_id(path: &Path) -> u64 {
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let mut hash = 0xcbf29ce484222325u64;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn path_id_to_ino(path_id: u64) -> i64 {
    (path_id & (i64::MAX as u64)) as i64
}

fn metadata_src_id(path: &Path, _metadata: &Metadata) -> SrcId {
    SrcId {
        path_id: stable_path_id(path),
    }
}

fn metadata_to_stats(metadata: &Metadata, ino: i64) -> Stats {
    let (atime, atime_nsec) = filetime_to_unix(metadata.last_access_time());
    let (mtime, mtime_nsec) = filetime_to_unix(metadata.last_write_time());
    let (ctime, ctime_nsec) = filetime_to_unix(metadata.creation_time());

    Stats {
        ino,
        mode: mode_from_metadata(metadata),
        nlink: 1,
        uid: 0,
        gid: 0,
        size: metadata.file_size() as i64,
        atime,
        mtime,
        ctime,
        atime_nsec,
        mtime_nsec,
        ctime_nsec,
        rdev: 0,
    }
}

fn to_system_time(tc: TimeChange) -> Option<SystemTime> {
    match tc {
        TimeChange::Omit => None,
        TimeChange::Now => Some(SystemTime::now()),
        TimeChange::Set(secs, nsec) => {
            if secs < 0 {
                Some(UNIX_EPOCH)
            } else {
                Some(UNIX_EPOCH + Duration::new(secs as u64, nsec))
            }
        }
    }
}

impl HostFS {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.exists() {
            return Err(Error::BaseDirectoryNotFound(root.display().to_string()));
        }
        if !root.is_dir() {
            return Err(Error::NotADirectory(root.display().to_string()));
        }

        let root = root
            .canonicalize()
            .map_err(|e| Error::Internal(format!("failed to canonicalize root: {}", e)))?;
        let metadata = fs::symlink_metadata(&root)?;
        let src_id = metadata_src_id(&root, &metadata);

        let mut inodes = HashMap::new();
        inodes.insert(
            ROOT_INO,
            Inode {
                path: root.clone(),
                src_id,
                nlookup: AtomicU64::new(1),
            },
        );

        let mut src_to_ino = HashMap::new();
        src_to_ino.insert(src_id, ROOT_INO);

        Ok(Self {
            root,
            inodes: RwLock::new(inodes),
            src_to_ino: RwLock::new(src_to_ino),
            next_ino: AtomicU64::new(2),
        })
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    fn get_inode_path(&self, ino: i64) -> Result<PathBuf> {
        let inodes = self.inodes.read().unwrap();
        let inode = inodes.get(&ino).ok_or(FsError::NotFound)?;
        Ok(inode.path.clone())
    }

    fn lstat_path(path: &Path) -> Result<Metadata> {
        Ok(fs::symlink_metadata(path)?)
    }

    fn open_path(path: &Path, flags: i32) -> Result<StdFile> {
        const O_ACCMODE: i32 = 0x0003;
        let access_mode = flags & O_ACCMODE;
        let read = access_mode != libc::O_WRONLY;
        let write = access_mode != libc::O_RDONLY;
        let append = (flags & libc::O_APPEND) != 0;
        let create = (flags & libc::O_CREAT) != 0;
        let truncate = (flags & libc::O_TRUNC) != 0;
        let create_new = create && (flags & libc::O_EXCL) != 0;

        let mut opts = OpenOptions::new();
        opts.read(read)
            .write(write || append)
            .append(append)
            .create(create && !create_new)
            .create_new(create_new)
            .truncate(truncate);

        Ok(opts.open(path)?)
    }

    fn get_or_create_inode(&self, path: PathBuf, metadata: &Metadata) -> (i64, bool) {
        let src_id = metadata_src_id(&path, metadata);

        {
            let src_map = self.src_to_ino.read().unwrap();
            if let Some(&ino) = src_map.get(&src_id) {
                let inodes = self.inodes.read().unwrap();
                if let Some(inode) = inodes.get(&ino) {
                    inode.nlookup.fetch_add(1, Ordering::Relaxed);
                    return (ino, false);
                }
            }
        }

        let preferred_ino = path_id_to_ino(src_id.path_id);
        let ino = if preferred_ino <= ROOT_INO {
            self.next_ino.fetch_add(1, Ordering::Relaxed) as i64
        } else {
            let inodes = self.inodes.read().unwrap();
            if inodes.contains_key(&preferred_ino) {
                self.next_ino.fetch_add(1, Ordering::Relaxed) as i64
            } else {
                preferred_ino
            }
        };
        let inode = Inode {
            path,
            src_id,
            nlookup: AtomicU64::new(1),
        };

        {
            let mut inodes = self.inodes.write().unwrap();
            inodes.insert(ino, inode);
        }
        {
            let mut src_map = self.src_to_ino.write().unwrap();
            src_map.insert(src_id, ino);
        }

        (ino, true)
    }

    fn apply_readonly_mode(path: &Path, mode: u32) -> Result<()> {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly((mode & 0o200) == 0);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    fn update_cached_paths_for_rename(&self, old_path: &Path, new_path: &Path) {
        let mut inodes = self.inodes.write().unwrap();
        for inode in inodes.values_mut() {
            if inode.path == old_path {
                inode.path = new_path.to_path_buf();
                continue;
            }

            if let Ok(suffix) = inode.path.strip_prefix(old_path) {
                inode.path = new_path.join(suffix);
            }
        }
    }

    fn choose_symlink_target_kind(target: &str) -> bool {
        let target_path = Path::new(target);
        if let Ok(metadata) = fs::metadata(target_path) {
            return metadata.is_dir();
        }
        target.ends_with('/') || target.ends_with('\\')
    }

    fn accumulate_stats(path: &Path, inodes: &mut u64, bytes_used: &mut u64) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        *inodes = inodes.saturating_add(1);
        if metadata.is_file() {
            *bytes_used = bytes_used.saturating_add(metadata.file_size());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                Self::accumulate_stats(&entry.path(), inodes, bytes_used)?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FileSystem for HostFS {
    async fn lookup(&self, parent_ino: i64, name: &str) -> Result<Option<Stats>> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let child_path = parent_path.join(name);
        let metadata = match Self::lstat_path(&child_path) {
            Ok(metadata) => metadata,
            Err(Error::Io(ref io_err)) if io_err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        let (ino, _) = self.get_or_create_inode(child_path, &metadata);
        Ok(Some(metadata_to_stats(&metadata, ino)))
    }

    async fn getattr(&self, ino: i64) -> Result<Option<Stats>> {
        let path = match self.get_inode_path(ino) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let metadata = Self::lstat_path(&path)?;
        Ok(Some(metadata_to_stats(&metadata, ino)))
    }

    async fn readlink(&self, ino: i64) -> Result<Option<String>> {
        let path = match self.get_inode_path(ino) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        match fs::read_link(&path) {
            Ok(target) => Ok(Some(target.to_string_lossy().replace('\\', "/"))),
            Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
                Err(FsError::NotASymlink.into())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    async fn readdir(&self, ino: i64) -> Result<Option<Vec<String>>> {
        let path = match self.get_inode_path(ino) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };

        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            entries.push(entry.file_name().to_string_lossy().to_string());
        }
        entries.sort();
        Ok(Some(entries))
    }

    async fn readdir_plus(&self, ino: i64) -> Result<Option<Vec<DirEntry>>> {
        let path = match self.get_inode_path(ino) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };

        let mut result = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let child_path = entry.path();
            let metadata = fs::symlink_metadata(&child_path)?;
            let (child_ino, _) = self.get_or_create_inode(child_path, &metadata);
            result.push(DirEntry {
                name,
                stats: metadata_to_stats(&metadata, child_ino),
            });
        }

        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(result))
    }

    async fn chmod(&self, ino: i64, mode: u32) -> Result<()> {
        let path = self.get_inode_path(ino)?;
        Self::apply_readonly_mode(&path, mode)
    }

    async fn chown(&self, _ino: i64, _uid: Option<u32>, _gid: Option<u32>) -> Result<()> {
        Ok(())
    }

    async fn utimens(&self, ino: i64, atime: TimeChange, mtime: TimeChange) -> Result<()> {
        let path = self.get_inode_path(ino)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Ok(());
        }

        let mut times = fs::FileTimes::new();
        let mut changed = false;
        if let Some(atime) = to_system_time(atime) {
            times = times.set_accessed(atime);
            changed = true;
        }
        if let Some(mtime) = to_system_time(mtime) {
            times = times.set_modified(mtime);
            changed = true;
        }
        if !changed {
            return Ok(());
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        file.set_times(times)?;
        Ok(())
    }

    async fn open(&self, ino: i64, flags: i32) -> Result<BoxedFile> {
        let path = self.get_inode_path(ino)?;
        let file = Self::open_path(&path, flags)?;
        Ok(Arc::new(HostFSFile { file, ino }))
    }

    async fn mkdir(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        _uid: u32,
        _gid: u32,
    ) -> Result<Stats> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let new_path = parent_path.join(name);
        match fs::create_dir(&new_path) {
            Ok(()) => {
                let _ = Self::apply_readonly_mode(&new_path, mode);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FsError::AlreadyExists.into());
            }
            Err(err) => return Err(err.into()),
        }

        self.lookup(parent_ino, name)
            .await?
            .ok_or(FsError::NotFound.into())
    }

    async fn create_file(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        _uid: u32,
        _gid: u32,
    ) -> Result<(Stats, BoxedFile)> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let new_path = parent_path.join(name);
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&new_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FsError::AlreadyExists.into());
            }
            Err(err) => return Err(err.into()),
        };

        let _ = Self::apply_readonly_mode(&new_path, mode);
        let metadata = fs::symlink_metadata(&new_path)?;
        let (ino, _) = self.get_or_create_inode(new_path, &metadata);
        let stats = metadata_to_stats(&metadata, ino);
        Ok((stats, Arc::new(HostFSFile { file, ino })))
    }

    async fn mknod(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        _rdev: u64,
        uid: u32,
        gid: u32,
    ) -> Result<Stats> {
        match mode & S_IFMT {
            0 | S_IFREG => self
                .create_file(parent_ino, name, mode, uid, gid)
                .await
                .map(|(stats, _)| stats),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Windows HostFS does not support special-file mknod",
            )
            .into()),
        }
    }

    async fn symlink(
        &self,
        parent_ino: i64,
        name: &str,
        target: &str,
        _uid: u32,
        _gid: u32,
    ) -> Result<Stats> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let new_path = parent_path.join(name);
        let create_dir_link = Self::choose_symlink_target_kind(target);

        let result = if create_dir_link {
            std::os::windows::fs::symlink_dir(target, &new_path)
        } else {
            std::os::windows::fs::symlink_file(target, &new_path)
        };

        match result {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FsError::AlreadyExists.into());
            }
            Err(err) => return Err(err.into()),
        }

        self.lookup(parent_ino, name)
            .await?
            .ok_or(FsError::NotFound.into())
    }

    async fn unlink(&self, parent_ino: i64, name: &str) -> Result<()> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let path = parent_path.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Err(FsError::IsADirectory.into());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound.into());
            }
            Err(err) => return Err(err.into()),
        }

        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(FsError::NotFound.into()),
            Err(err) => Err(err.into()),
        }
    }

    async fn rmdir(&self, parent_ino: i64, name: &str) -> Result<()> {
        let parent_path = self.get_inode_path(parent_ino)?;
        let path = parent_path.join(name);
        match fs::remove_dir(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(FsError::NotFound.into()),
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                Err(FsError::NotEmpty.into())
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn link(&self, ino: i64, newparent_ino: i64, newname: &str) -> Result<Stats> {
        let path = self.get_inode_path(ino)?;
        let newparent_path = self.get_inode_path(newparent_ino)?;
        let new_path = newparent_path.join(newname);
        match fs::hard_link(&path, &new_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FsError::AlreadyExists.into());
            }
            Err(err) => return Err(err.into()),
        }

        self.getattr(ino).await?.ok_or(FsError::NotFound.into())
    }

    async fn rename(
        &self,
        oldparent_ino: i64,
        oldname: &str,
        newparent_ino: i64,
        newname: &str,
    ) -> Result<()> {
        let oldparent_path = self.get_inode_path(oldparent_ino)?;
        let newparent_path = self.get_inode_path(newparent_ino)?;
        let old_path = oldparent_path.join(oldname);
        let new_path = newparent_path.join(newname);

        match fs::rename(&old_path, &new_path) {
            Ok(()) => {
                self.update_cached_paths_for_rename(&old_path, &new_path);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(FsError::NotFound.into()),
            Err(err) => Err(err.into()),
        }
    }

    async fn statfs(&self) -> Result<FilesystemStats> {
        let mut inodes = 0u64;
        let mut bytes_used = 0u64;
        Self::accumulate_stats(&self.root, &mut inodes, &mut bytes_used)?;
        Ok(FilesystemStats { inodes, bytes_used })
    }

    async fn forget(&self, ino: i64, nlookup: u64) {
        if ino == ROOT_INO {
            return;
        }

        let should_remove = {
            let inodes = self.inodes.read().unwrap();
            if let Some(inode) = inodes.get(&ino) {
                let old = inode.nlookup.fetch_sub(nlookup, Ordering::Relaxed);
                old <= nlookup
            } else {
                false
            }
        };

        if should_remove {
            let mut inodes = self.inodes.write().unwrap();
            if let Some(inode) = inodes.remove(&ino) {
                let mut src_map = self.src_to_ino.write().unwrap();
                src_map.remove(&inode.src_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_FILE_MODE;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_hostfs_basic() -> Result<()> {
        let dir = tempdir()?;
        let fs = HostFS::new(dir.path())?;

        let (_, file) = fs
            .create_file(ROOT_INO, "test.txt", DEFAULT_FILE_MODE, 0, 0)
            .await?;
        file.pwrite(0, b"hello world").await?;

        let stats = fs.lookup(ROOT_INO, "test.txt").await?.unwrap();
        assert!(stats.is_file());

        let file = fs.open(stats.ino, libc::O_RDONLY).await?;
        let data = file.pread(0, 100).await?;
        assert_eq!(data, b"hello world");

        Ok(())
    }

    #[tokio::test]
    async fn test_hostfs_readdir_plus_sees_base_entries() -> Result<()> {
        let dir = tempdir()?;
        fs::create_dir(dir.path().join("nested"))?;
        fs::write(dir.path().join("nested").join("a.txt"), b"a")?;
        fs::write(dir.path().join("nested").join("b.txt"), b"b")?;

        let fs = HostFS::new(dir.path())?;
        let nested = fs.lookup(ROOT_INO, "nested").await?.unwrap();
        let entries = fs.readdir_plus(nested.ino).await?.unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "b.txt");
        assert!(entries.iter().all(|entry| entry.stats.is_file()));

        Ok(())
    }
}
