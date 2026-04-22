use super::schema::{parse_compose_doc, AppfsComposeDoc};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_COMPOSE_FILENAMES: [&str; 2] = ["appfs-compose.yaml", "appfs-compose.yml"];

pub(crate) fn discover_compose_file(cwd: &Path) -> Result<PathBuf> {
    let mut matches = DEFAULT_COMPOSE_FILENAMES
        .iter()
        .map(|filename| cwd.join(filename))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();

    match matches.len() {
        0 => anyhow::bail!(
            "no AppFS compose file found in {}; tried {}",
            cwd.display(),
            DEFAULT_COMPOSE_FILENAMES.join(", ")
        ),
        1 => Ok(matches.remove(0)),
        _ => anyhow::bail!(
            "multiple AppFS compose files found in {}: {}",
            cwd.display(),
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub(crate) fn load_compose_doc(compose_path: Option<&Path>, cwd: &Path) -> Result<AppfsComposeDoc> {
    let resolved_path = match compose_path {
        Some(path) => resolve_source_path(path, cwd),
        None => discover_compose_file(cwd)?,
    };
    load_compose_doc_from_path(&resolved_path)
}

pub(crate) fn load_compose_doc_from_path(path: &Path) -> Result<AppfsComposeDoc> {
    let source_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current working directory for compose path")?
            .join(path)
    };
    let yaml = fs::read_to_string(&source_path).with_context(|| {
        format!(
            "failed to read AppFS compose file {}",
            source_path.display()
        )
    })?;
    parse_compose_doc(&yaml, source_path)
}

fn resolve_source_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{discover_compose_file, load_compose_doc, DEFAULT_COMPOSE_FILENAMES};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn discovers_default_compose_filename() {
        let temp = TempDir::new().expect("tempdir");
        let compose_path = temp.path().join(DEFAULT_COMPOSE_FILENAMES[0]);
        fs::write(
            &compose_path,
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
"#,
        )
        .expect("write compose");

        let discovered = discover_compose_file(temp.path()).expect("should discover compose");
        assert_eq!(discovered, compose_path);
    }

    #[test]
    fn rejects_ambiguous_default_compose_filenames() {
        let temp = TempDir::new().expect("tempdir");
        for filename in DEFAULT_COMPOSE_FILENAMES {
            fs::write(
                temp.path().join(filename),
                r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
"#,
            )
            .expect("write compose");
        }

        let err = discover_compose_file(temp.path()).expect_err("should fail");
        assert!(err.to_string().contains("multiple AppFS compose files"));
    }

    #[test]
    fn load_compose_doc_resolves_explicit_relative_path_against_cwd() {
        let temp = TempDir::new().expect("tempdir");
        let compose_dir = temp.path().join("config");
        fs::create_dir_all(&compose_dir).expect("create compose dir");
        fs::write(
            compose_dir.join("appfs-compose.yaml"),
            r#"
version: 1
runtime:
  db: ../.agentfs/demo.db
  mountpoint: ../mnt/appfs
  backend: fuse
"#,
        )
        .expect("write compose");

        let doc = load_compose_doc(
            Some(std::path::Path::new("config/appfs-compose.yaml")),
            temp.path(),
        )
        .expect("compose should load");

        assert_eq!(doc.source_path, compose_dir.join("appfs-compose.yaml"));
        assert_eq!(doc.runtime.db, temp.path().join(".agentfs/demo.db"));
        assert_eq!(doc.runtime.mountpoint, temp.path().join("mnt/appfs"));
    }
}
