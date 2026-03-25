use super::{AppfsBridgeCliArgs, AppfsRuntimeCliArgs, ResolvedAppfsRuntimeCliArgs};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const APPFS_REGISTRY_VERSION: u32 = 1;
pub(crate) const APPFS_REGISTRY_REL_PATH: &str = "_appfs/apps.registry.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsAppsRegistryDoc {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) apps: Vec<AppfsRegisteredAppDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRegisteredAppDoc {
    pub(crate) app_id: String,
    pub(crate) transport: AppfsRegistryTransportDoc,
    pub(crate) session_id: String,
    pub(crate) registered_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRegistryTransportDoc {
    pub(crate) kind: AppfsRegistryTransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) endpoint: Option<String>,
    pub(crate) http_timeout_ms: u64,
    pub(crate) grpc_timeout_ms: u64,
    pub(crate) bridge_max_retries: u32,
    pub(crate) bridge_initial_backoff_ms: u64,
    pub(crate) bridge_max_backoff_ms: u64,
    pub(crate) bridge_circuit_breaker_failures: u32,
    pub(crate) bridge_circuit_breaker_cooldown_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppfsRegistryTransportKind {
    InProcess,
    Http,
    Grpc,
}

pub(crate) fn app_registry_path(root: &Path) -> PathBuf {
    root.join(APPFS_REGISTRY_REL_PATH.replace('/', std::path::MAIN_SEPARATOR_STR))
}

pub(crate) fn parse_app_registry_bytes(bytes: &[u8]) -> Result<AppfsAppsRegistryDoc> {
    let doc: AppfsAppsRegistryDoc =
        serde_json::from_slice(bytes).context("failed to parse AppFS app registry JSON")?;
    validate_app_registry(&doc)?;
    Ok(doc)
}

pub(crate) fn read_app_registry(root: &Path) -> Result<Option<AppfsAppsRegistryDoc>> {
    let path = app_registry_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read AppFS app registry {}", path.display()))?;
    parse_app_registry_bytes(&bytes).map(Some)
}

pub(crate) fn write_app_registry(root: &Path, doc: &AppfsAppsRegistryDoc) -> Result<()> {
    validate_app_registry(doc)?;
    let path = app_registry_path(root);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid registry path {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create AppFS registry directory {}",
            parent.display()
        )
    })?;
    let tmp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(doc).context("failed to serialize AppFS app registry")?;
    fs::write(&tmp_path, bytes).with_context(|| {
        format!(
            "failed to write temporary AppFS registry {}",
            tmp_path.display()
        )
    })?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| {
            format!(
                "failed to replace existing AppFS registry {}",
                path.display()
            )
        })?;
    }
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("failed to publish AppFS registry {}", path.display()))?;
    Ok(())
}

pub(crate) fn build_app_registry_doc(
    runtime_args: &[ResolvedAppfsRuntimeCliArgs],
    active_scopes: &HashMap<String, Option<String>>,
    existing: Option<&AppfsAppsRegistryDoc>,
) -> AppfsAppsRegistryDoc {
    let existing_registered_at = existing
        .map(|doc| {
            doc.apps
                .iter()
                .map(|app| (app.app_id.clone(), app.registered_at.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    AppfsAppsRegistryDoc {
        version: APPFS_REGISTRY_VERSION,
        apps: runtime_args
            .iter()
            .map(|runtime| AppfsRegisteredAppDoc {
                app_id: runtime.app_id.clone(),
                transport: transport_doc_from_bridge_args(&runtime.bridge),
                session_id: runtime.session_id.clone(),
                registered_at: existing_registered_at
                    .get(&runtime.app_id)
                    .cloned()
                    .unwrap_or_else(|| now.clone()),
                active_scope: active_scopes.get(&runtime.app_id).cloned().flatten(),
            })
            .collect(),
    }
}

pub(crate) fn runtime_args_from_registry(
    doc: &AppfsAppsRegistryDoc,
) -> Result<Vec<AppfsRuntimeCliArgs>> {
    validate_app_registry(doc)?;
    Ok(doc
        .apps
        .iter()
        .map(|app| AppfsRuntimeCliArgs {
            app_id: app.app_id.clone(),
            session_id: Some(app.session_id.clone()),
            bridge: bridge_args_from_transport_doc(&app.transport),
        })
        .collect())
}

fn transport_doc_from_bridge_args(args: &AppfsBridgeCliArgs) -> AppfsRegistryTransportDoc {
    let (kind, endpoint) = if let Some(endpoint) = args.adapter_http_endpoint.clone() {
        (AppfsRegistryTransportKind::Http, Some(endpoint))
    } else if let Some(endpoint) = args.adapter_grpc_endpoint.clone() {
        (AppfsRegistryTransportKind::Grpc, Some(endpoint))
    } else {
        (AppfsRegistryTransportKind::InProcess, None)
    };
    AppfsRegistryTransportDoc {
        kind,
        endpoint,
        http_timeout_ms: args.adapter_http_timeout_ms,
        grpc_timeout_ms: args.adapter_grpc_timeout_ms,
        bridge_max_retries: args.adapter_bridge_max_retries,
        bridge_initial_backoff_ms: args.adapter_bridge_initial_backoff_ms,
        bridge_max_backoff_ms: args.adapter_bridge_max_backoff_ms,
        bridge_circuit_breaker_failures: args.adapter_bridge_circuit_breaker_failures,
        bridge_circuit_breaker_cooldown_ms: args.adapter_bridge_circuit_breaker_cooldown_ms,
    }
}

fn bridge_args_from_transport_doc(doc: &AppfsRegistryTransportDoc) -> AppfsBridgeCliArgs {
    let (adapter_http_endpoint, adapter_grpc_endpoint) = match doc.kind {
        AppfsRegistryTransportKind::InProcess => (None, None),
        AppfsRegistryTransportKind::Http => (doc.endpoint.clone(), None),
        AppfsRegistryTransportKind::Grpc => (None, doc.endpoint.clone()),
    };
    AppfsBridgeCliArgs {
        adapter_http_endpoint,
        adapter_http_timeout_ms: doc.http_timeout_ms,
        adapter_grpc_endpoint,
        adapter_grpc_timeout_ms: doc.grpc_timeout_ms,
        adapter_bridge_max_retries: doc.bridge_max_retries,
        adapter_bridge_initial_backoff_ms: doc.bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms: doc.bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures: doc.bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms: doc.bridge_circuit_breaker_cooldown_ms,
    }
}

fn validate_app_registry(doc: &AppfsAppsRegistryDoc) -> Result<()> {
    if doc.version != APPFS_REGISTRY_VERSION {
        anyhow::bail!(
            "unsupported AppFS app registry version {} (expected {})",
            doc.version,
            APPFS_REGISTRY_VERSION
        );
    }
    let mut seen = HashMap::new();
    for app in &doc.apps {
        if app.app_id.trim().is_empty() {
            anyhow::bail!("registry app_id cannot be empty");
        }
        if app.session_id.trim().is_empty() {
            anyhow::bail!("registry session_id cannot be empty for app {}", app.app_id);
        }
        if seen.insert(app.app_id.clone(), ()).is_some() {
            anyhow::bail!("duplicate registry app_id {}", app.app_id);
        }
        match app.transport.kind {
            AppfsRegistryTransportKind::InProcess => {
                if app.transport.endpoint.is_some() {
                    anyhow::bail!(
                        "registry transport endpoint must be empty for in_process app {}",
                        app.app_id
                    );
                }
            }
            AppfsRegistryTransportKind::Http | AppfsRegistryTransportKind::Grpc => {
                if app
                    .transport
                    .endpoint
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    anyhow::bail!(
                        "registry transport endpoint is required for managed app {}",
                        app.app_id
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        app_registry_path, build_app_registry_doc, parse_app_registry_bytes, read_app_registry,
        runtime_args_from_registry, write_app_registry,
    };
    use crate::cmd::appfs::{AppfsBridgeCliArgs, ResolvedAppfsRuntimeCliArgs};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn bridge_args_http() -> AppfsBridgeCliArgs {
        AppfsBridgeCliArgs {
            adapter_http_endpoint: Some("http://127.0.0.1:8080".to_string()),
            adapter_http_timeout_ms: 5000,
            adapter_grpc_endpoint: None,
            adapter_grpc_timeout_ms: 5000,
            adapter_bridge_max_retries: 2,
            adapter_bridge_initial_backoff_ms: 100,
            adapter_bridge_max_backoff_ms: 1000,
            adapter_bridge_circuit_breaker_failures: 5,
            adapter_bridge_circuit_breaker_cooldown_ms: 3000,
        }
    }

    #[test]
    fn registry_roundtrip_preserves_bridge_and_session() {
        let temp = TempDir::new().expect("tempdir");
        let runtimes = vec![ResolvedAppfsRuntimeCliArgs {
            app_id: "aiim".to_string(),
            session_id: "sess-aiim".to_string(),
            bridge: bridge_args_http(),
        }];
        let mut active_scopes = HashMap::new();
        active_scopes.insert("aiim".to_string(), Some("chat-001".to_string()));
        let doc = build_app_registry_doc(&runtimes, &active_scopes, None);

        write_app_registry(temp.path(), &doc).expect("write registry");
        let stored = read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert_eq!(stored.apps.len(), 1);
        assert_eq!(stored.apps[0].app_id, "aiim");
        assert_eq!(stored.apps[0].session_id, "sess-aiim");
        assert_eq!(stored.apps[0].active_scope.as_deref(), Some("chat-001"));

        let runtime_args = runtime_args_from_registry(&stored).expect("runtime args");
        assert_eq!(runtime_args.len(), 1);
        assert_eq!(runtime_args[0].app_id, "aiim");
        assert_eq!(
            runtime_args[0].bridge.adapter_http_endpoint.as_deref(),
            Some("http://127.0.0.1:8080")
        );
    }

    #[test]
    fn registry_rejects_corrupt_payload() {
        let err = parse_app_registry_bytes(br#"{"version":1,"apps":[{"app_id":"","session_id":"s","registered_at":"2026-03-25T00:00:00Z","transport":{"kind":"in_process","http_timeout_ms":1,"grpc_timeout_ms":1,"bridge_max_retries":1,"bridge_initial_backoff_ms":1,"bridge_max_backoff_ms":1,"bridge_circuit_breaker_failures":1,"bridge_circuit_breaker_cooldown_ms":1}}]}"#)
            .expect_err("invalid registry should fail");
        assert!(err.to_string().contains("app_id cannot be empty"));
    }

    #[test]
    fn registry_path_uses_appfs_namespace() {
        let path = app_registry_path(std::path::Path::new("/tmp/app"));
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with("/_appfs/apps.registry.json"));
    }
}
