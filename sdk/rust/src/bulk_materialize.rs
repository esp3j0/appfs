use crate::filesystem::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BulkMaterializePlan {
    pub entries: Vec<BulkMaterializeEntry>,
}

impl BulkMaterializePlan {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: BulkMaterializeEntry) {
        self.entries.push(entry);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkMaterializeEntry {
    EnsureDir {
        path: String,
        uid: u32,
        gid: u32,
        mode: u32,
    },
    EnsureEmptyFile {
        path: String,
        uid: u32,
        gid: u32,
        mode: u32,
    },
    WriteFile {
        path: String,
        uid: u32,
        gid: u32,
        mode: u32,
        bytes: Vec<u8>,
    },
}

impl BulkMaterializeEntry {
    pub fn ensure_dir(path: impl Into<String>) -> Self {
        Self::EnsureDir {
            path: path.into(),
            uid: 0,
            gid: 0,
            mode: DEFAULT_DIR_MODE,
        }
    }

    pub fn ensure_empty_file(path: impl Into<String>) -> Self {
        Self::EnsureEmptyFile {
            path: path.into(),
            uid: 0,
            gid: 0,
            mode: DEFAULT_FILE_MODE,
        }
    }

    pub fn write_file(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self::WriteFile {
            path: path.into(),
            uid: 0,
            gid: 0,
            mode: DEFAULT_FILE_MODE,
            bytes,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::EnsureDir { path, .. }
            | Self::EnsureEmptyFile { path, .. }
            | Self::WriteFile { path, .. } => path,
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.path()
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
    }

    pub(crate) fn sort_rank(&self) -> u8 {
        match self {
            Self::EnsureDir { .. } => 0,
            Self::EnsureEmptyFile { .. } | Self::WriteFile { .. } => 1,
        }
    }
}
