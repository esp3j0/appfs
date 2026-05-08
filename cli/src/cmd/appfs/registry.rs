use super::{AppfsBridgeCliArgs, AppfsRuntimeCliArgs, ResolvedAppfsRuntimeCliArgs};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const APPFS_REGISTRY_VERSION: u32 = 1;
pub(crate) const APPFS_REGISTRY_REL_PATH: &str = "_appfs/apps.registry.json";
pub(crate) const APPFS_APP_POLICY_REGISTRY_REL_PATH: &str = "_appfs/app-policies.registry.json";
pub(crate) const APPFS_PRINCIPAL_REGISTRY_REL_PATH: &str = "_appfs/principals.registry.json";
pub(crate) const APPFS_DEFAULT_PRINCIPAL_ID: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsAppsRegistryDoc {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) apps: Vec<AppfsRegisteredAppDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRegisteredAppDoc {
    pub(crate) instance_id: String,
    pub(crate) app_id: String,
    pub(crate) visibility: AppfsRegisteredAppVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) principal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile_id: Option<String>,
    pub(crate) path: String,
    pub(crate) transport: AppfsRegistryTransportDoc,
    pub(crate) session_id: String,
    pub(crate) registered_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppfsRegisteredAppVisibility {
    Public,
    PrivateInstance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsAppPolicyRegistryDoc {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) apps: Vec<AppfsAppPolicyRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsAppPolicyRecord {
    pub(crate) app_id: String,
    pub(crate) visibility: AppfsAppPolicyVisibility,
    pub(crate) connector: String,
    pub(crate) transport: AppfsRegistryTransportDoc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential_policy: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppfsAppPolicyVisibility {
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct PrincipalRegistryDoc {
    pub(crate) version: u32,
    pub(crate) default_principal_id: String,
    #[serde(default)]
    pub(crate) principals: Vec<PrincipalRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct PrincipalRecord {
    pub(crate) principal_id: String,
    pub(crate) display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) kind: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
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

pub(crate) fn app_policy_registry_path(root: &Path) -> PathBuf {
    root.join(APPFS_APP_POLICY_REGISTRY_REL_PATH.replace('/', std::path::MAIN_SEPARATOR_STR))
}

pub(crate) fn principal_registry_path(root: &Path) -> PathBuf {
    root.join(APPFS_PRINCIPAL_REGISTRY_REL_PATH.replace('/', std::path::MAIN_SEPARATOR_STR))
}

pub(crate) fn principal_record_path(root: &Path, principal_id: &str) -> PathBuf {
    root.join("_appfs")
        .join("principals")
        .join(format!("{principal_id}.res.json"))
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

pub(crate) fn read_principal_registry(root: &Path) -> Result<Option<PrincipalRegistryDoc>> {
    let path = principal_registry_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read AppFS principal registry {}", path.display()))?;
    let doc: PrincipalRegistryDoc =
        serde_json::from_slice(&bytes).context("failed to parse AppFS principal registry JSON")?;
    validate_principal_registry(&doc)?;
    Ok(Some(doc))
}

pub(crate) fn read_app_policy_registry(root: &Path) -> Result<Option<AppfsAppPolicyRegistryDoc>> {
    let path = app_policy_registry_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "failed to read AppFS app policy registry {}",
            path.display()
        )
    })?;
    let doc: AppfsAppPolicyRegistryDoc =
        serde_json::from_slice(&bytes).context("failed to parse AppFS app policy registry JSON")?;
    validate_app_policy_registry(&doc)?;
    Ok(Some(doc))
}

pub(crate) fn write_principal_registry(root: &Path, doc: &PrincipalRegistryDoc) -> Result<()> {
    validate_principal_registry(doc)?;
    let path = principal_registry_path(root);
    write_pretty_json_file(&path, doc, "AppFS principal registry")
}

pub(crate) fn write_principal_record_view(root: &Path, record: &PrincipalRecord) -> Result<()> {
    let path = principal_record_path(root, &record.principal_id);
    write_pretty_json_file(&path, record, "AppFS principal record")
}

pub(crate) fn delete_principal_record_view(root: &Path, principal_id: &str) -> Result<()> {
    let path = principal_record_path(root, principal_id);
    if path.exists() {
        fs::remove_file(&path).with_context(|| {
            format!(
                "failed to remove AppFS principal record view {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn write_app_policy_registry(
    root: &Path,
    doc: &AppfsAppPolicyRegistryDoc,
) -> Result<()> {
    validate_app_policy_registry(doc)?;
    let path = app_policy_registry_path(root);
    write_pretty_json_file(&path, doc, "AppFS app policy registry")
}

pub(crate) fn write_app_registry(root: &Path, doc: &AppfsAppsRegistryDoc) -> Result<()> {
    validate_app_registry(doc)?;
    let path = app_registry_path(root);
    write_pretty_json_file(&path, doc, "AppFS app registry")
}

fn write_pretty_json_file<T: Serialize>(path: &Path, doc: &T, label: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid registry path {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {label} directory {}", parent.display()))?;
    let tmp_path = path.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec_pretty(doc).with_context(|| format!("failed to serialize {label}"))?;
    fs::write(&tmp_path, bytes)
        .with_context(|| format!("failed to write temporary {label} {}", tmp_path.display()))?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to replace existing {label} {}", path.display()))?;
    }
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("failed to publish {label} {}", path.display()))?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn build_app_registry_doc(
    runtime_args: &[ResolvedAppfsRuntimeCliArgs],
    active_scopes: &HashMap<String, Option<String>>,
    existing: Option<&AppfsAppsRegistryDoc>,
) -> AppfsAppsRegistryDoc {
    let existing_registered_at = existing
        .map(|doc| {
            doc.apps
                .iter()
                .map(|app| (app.instance_id.clone(), app.registered_at.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    AppfsAppsRegistryDoc {
        version: APPFS_REGISTRY_VERSION,
        apps: runtime_args
            .iter()
            .map(|runtime| AppfsRegisteredAppDoc {
                instance_id: runtime.app_id.clone(),
                app_id: runtime.app_id.clone(),
                visibility: AppfsRegisteredAppVisibility::Public,
                parent_app_id: None,
                principal_id: None,
                profile_id: None,
                path: runtime.app_id.clone(),
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

pub(crate) fn transport_doc_from_bridge_args(
    args: &AppfsBridgeCliArgs,
) -> AppfsRegistryTransportDoc {
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

pub(crate) fn bridge_args_from_transport_doc(
    doc: &AppfsRegistryTransportDoc,
) -> AppfsBridgeCliArgs {
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
        if app.instance_id.trim().is_empty() {
            anyhow::bail!("registry instance_id cannot be empty");
        }
        if app.app_id.trim().is_empty() {
            anyhow::bail!("registry app_id cannot be empty");
        }
        if app.path.trim().is_empty() {
            anyhow::bail!("registry path cannot be empty for app {}", app.app_id);
        }
        if app.session_id.trim().is_empty() {
            anyhow::bail!("registry session_id cannot be empty for app {}", app.app_id);
        }
        if seen.insert(app.instance_id.clone(), ()).is_some() {
            anyhow::bail!("duplicate registry instance_id {}", app.instance_id);
        }
        match app.visibility {
            AppfsRegisteredAppVisibility::Public => {
                if app.principal_id.is_some()
                    || app.profile_id.is_some()
                    || app.parent_app_id.is_some()
                {
                    anyhow::bail!(
                        "public registry app {} cannot define private instance identity fields",
                        app.app_id
                    );
                }
            }
            AppfsRegisteredAppVisibility::PrivateInstance => {
                if app
                    .principal_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    anyhow::bail!("private registry app {} requires principal_id", app.app_id);
                }
                if app
                    .parent_app_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    anyhow::bail!("private registry app {} requires parent_app_id", app.app_id);
                }
            }
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

fn validate_app_policy_registry(doc: &AppfsAppPolicyRegistryDoc) -> Result<()> {
    if doc.version != APPFS_REGISTRY_VERSION {
        anyhow::bail!(
            "unsupported AppFS app policy registry version {} (expected {})",
            doc.version,
            APPFS_REGISTRY_VERSION
        );
    }
    let mut seen = HashMap::new();
    for app in &doc.apps {
        if app.app_id.trim().is_empty() {
            anyhow::bail!("app policy app_id cannot be empty");
        }
        if app.connector.trim().is_empty() {
            anyhow::bail!(
                "app policy connector cannot be empty for app {}",
                app.app_id
            );
        }
        if seen.insert(app.app_id.clone(), ()).is_some() {
            anyhow::bail!("duplicate app policy app_id {}", app.app_id);
        }
        match app.visibility {
            AppfsAppPolicyVisibility::Public => {
                if app
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    anyhow::bail!("public app policy {} requires path", app.app_id);
                }
                if app.path_template.is_some() || app.profile_template.is_some() {
                    anyhow::bail!(
                        "public app policy {} cannot define private templates",
                        app.app_id
                    );
                }
            }
            AppfsAppPolicyVisibility::Private => {
                if app.path.is_some() {
                    anyhow::bail!("private app policy {} cannot define path", app.app_id);
                }
                let Some(path_template) = app.path_template.as_deref() else {
                    anyhow::bail!("private app policy {} requires path_template", app.app_id);
                };
                if !path_template.contains("{principal_id}") {
                    anyhow::bail!(
                        "private app policy {} path_template must contain {{principal_id}}",
                        app.app_id
                    );
                }
                if let Some(profile_template) = app.profile_template.as_deref() {
                    if !profile_template.contains("{principal_id}") {
                        anyhow::bail!(
                            "private app policy {} profile_template must contain {{principal_id}}",
                            app.app_id
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_principal_registry(doc: &PrincipalRegistryDoc) -> Result<()> {
    if doc.version != APPFS_REGISTRY_VERSION {
        anyhow::bail!(
            "unsupported AppFS principal registry version {} (expected {})",
            doc.version,
            APPFS_REGISTRY_VERSION
        );
    }
    validate_principal_id(&doc.default_principal_id)?;
    let mut seen = HashMap::new();
    for principal in &doc.principals {
        validate_principal_id(&principal.principal_id)?;
        if principal.display_name.trim().is_empty() {
            anyhow::bail!(
                "principal {} display_name cannot be empty",
                principal.principal_id
            );
        }
        if principal.kind.trim().is_empty() {
            anyhow::bail!("principal {} kind cannot be empty", principal.principal_id);
        }
        if principal.created_at.trim().is_empty() || principal.updated_at.trim().is_empty() {
            anyhow::bail!(
                "principal {} timestamps cannot be empty",
                principal.principal_id
            );
        }
        if seen.insert(principal.principal_id.clone(), ()).is_some() {
            anyhow::bail!("duplicate principal_id {}", principal.principal_id);
        }
    }
    Ok(())
}

pub(crate) fn validate_principal_id(principal_id: &str) -> Result<()> {
    if principal_id.trim() != principal_id || principal_id.is_empty() {
        anyhow::bail!("principal_id cannot be empty or contain leading/trailing whitespace");
    }
    if principal_id == "." || principal_id == ".." {
        anyhow::bail!("principal_id cannot be . or ..");
    }
    if principal_id.len() > 120 {
        anyhow::bail!("principal_id cannot exceed 120 bytes");
    }
    if principal_id.contains(['/', '\\', '\0', ':']) {
        anyhow::bail!("principal_id cannot contain path separators, NUL, or drive separators");
    }
    if !principal_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        anyhow::bail!("principal_id can only contain ASCII letters, digits, _ and -");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        app_registry_path, build_app_registry_doc, parse_app_registry_bytes, read_app_registry,
        runtime_args_from_registry, write_app_registry, AppfsRegisteredAppVisibility,
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
        assert_eq!(stored.apps[0].instance_id, "aiim");
        assert_eq!(stored.apps[0].app_id, "aiim");
        assert_eq!(
            stored.apps[0].visibility,
            AppfsRegisteredAppVisibility::Public
        );
        assert_eq!(stored.apps[0].path, "aiim");
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
        let err = parse_app_registry_bytes(br#"{"version":1,"apps":[{"instance_id":"bad","app_id":"","visibility":"public","path":"bad","session_id":"s","registered_at":"2026-03-25T00:00:00Z","transport":{"kind":"in_process","http_timeout_ms":1,"grpc_timeout_ms":1,"bridge_max_retries":1,"bridge_initial_backoff_ms":1,"bridge_max_backoff_ms":1,"bridge_circuit_breaker_failures":1,"bridge_circuit_breaker_cooldown_ms":1}}]}"#)
            .expect_err("invalid registry should fail");
        assert!(err.to_string().contains("app_id cannot be empty"));
    }

    #[test]
    fn registry_rejects_old_app_registry_format() {
        let err = parse_app_registry_bytes(br#"{"version":1,"apps":[{"app_id":"aiim","session_id":"sess-aiim","registered_at":"2026-03-25T00:00:00Z","active_scope":null,"transport":{"kind":"http","endpoint":"http://127.0.0.1:8080","http_timeout_ms":1,"grpc_timeout_ms":1,"bridge_max_retries":1,"bridge_initial_backoff_ms":1,"bridge_max_backoff_ms":1,"bridge_circuit_breaker_failures":1,"bridge_circuit_breaker_cooldown_ms":1}}]}"#)
            .expect_err("old registry should fail");
        assert!(format!("{err:#}").contains("missing field `instance_id`"));
    }

    #[test]
    fn registry_path_uses_appfs_namespace() {
        let path = app_registry_path(std::path::Path::new("/tmp/app"));
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with("/_appfs/apps.registry.json"));
    }
}
