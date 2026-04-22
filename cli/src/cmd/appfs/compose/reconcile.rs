use super::connector_supervisor::ResolvedComposeApp;
use crate::cmd::appfs::normalize_appfs_session_id;
use crate::cmd::appfs::registry;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub(crate) fn build_registry_doc_from_resolved_apps(
    resolved_apps: &BTreeMap<String, ResolvedComposeApp>,
    existing: Option<&registry::AppfsAppsRegistryDoc>,
) -> registry::AppfsAppsRegistryDoc {
    let existing_registered_at = existing
        .map(|doc| {
            doc.apps
                .iter()
                .map(|app| (app.app_id.clone(), app.registered_at.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();

    registry::AppfsAppsRegistryDoc {
        version: registry::APPFS_REGISTRY_VERSION,
        apps: resolved_apps
            .values()
            .map(|app| {
                let session_id = normalize_appfs_session_id(app.session_id.clone());
                registry::AppfsRegisteredAppDoc {
                    app_id: app.app_id.clone(),
                    transport: registry_transport_from_resolved_app(app),
                    session_id,
                    registered_at: existing_registered_at
                        .get(&app.app_id)
                        .cloned()
                        .unwrap_or_else(|| now.clone()),
                    active_scope: None,
                }
            })
            .collect(),
    }
}

pub(crate) fn bootstrap_registry_from_resolved_apps(
    root: &Path,
    resolved_apps: &BTreeMap<String, ResolvedComposeApp>,
) -> Result<registry::AppfsAppsRegistryDoc> {
    let existing = registry::read_app_registry(root)?;
    let doc = build_registry_doc_from_resolved_apps(resolved_apps, existing.as_ref());
    if existing.as_ref() != Some(&doc) {
        registry::write_app_registry(root, &doc)?;
    }
    Ok(doc)
}

fn registry_transport_from_resolved_app(
    app: &ResolvedComposeApp,
) -> registry::AppfsRegistryTransportDoc {
    let kind = match app.transport_kind {
        super::schema::AppfsComposeTransportKind::Http => {
            registry::AppfsRegistryTransportKind::Http
        }
        super::schema::AppfsComposeTransportKind::Grpc => {
            registry::AppfsRegistryTransportKind::Grpc
        }
    };
    registry::AppfsRegistryTransportDoc {
        kind,
        endpoint: Some(app.endpoint.clone()),
        http_timeout_ms: app.transport.http_timeout_ms,
        grpc_timeout_ms: app.transport.grpc_timeout_ms,
        bridge_max_retries: app.transport.bridge_max_retries,
        bridge_initial_backoff_ms: app.transport.bridge_initial_backoff_ms,
        bridge_max_backoff_ms: app.transport.bridge_max_backoff_ms,
        bridge_circuit_breaker_failures: app.transport.bridge_circuit_breaker_failures,
        bridge_circuit_breaker_cooldown_ms: app.transport.bridge_circuit_breaker_cooldown_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::{bootstrap_registry_from_resolved_apps, build_registry_doc_from_resolved_apps};
    use crate::cmd::appfs::compose::connector_supervisor::ResolvedComposeApp;
    use crate::cmd::appfs::compose::schema::{AppfsComposeAppTransport, AppfsComposeTransportKind};
    use crate::cmd::appfs::{registry, resolve_runtime_cli_args};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn resolved_app(
        app_id: &str,
        connector_name: &str,
        transport_kind: AppfsComposeTransportKind,
        endpoint: &str,
        session_id: Option<&str>,
    ) -> ResolvedComposeApp {
        ResolvedComposeApp {
            app_id: app_id.to_string(),
            connector_name: connector_name.to_string(),
            transport_kind,
            endpoint: endpoint.to_string(),
            session_id: session_id.map(str::to_string),
            transport: AppfsComposeAppTransport::default(),
        }
    }

    #[test]
    fn build_registry_doc_from_resolved_apps_keeps_only_declared_apps() {
        let mut resolved_apps = BTreeMap::new();
        resolved_apps.insert(
            "aiim".to_string(),
            resolved_app(
                "aiim",
                "aiim-http",
                AppfsComposeTransportKind::Http,
                "http://127.0.0.1:8080",
                Some("sess-aiim"),
            ),
        );

        let existing = registry::AppfsAppsRegistryDoc {
            version: registry::APPFS_REGISTRY_VERSION,
            apps: vec![
                registry::AppfsRegisteredAppDoc {
                    app_id: "aiim".to_string(),
                    transport: registry::AppfsRegistryTransportDoc {
                        kind: registry::AppfsRegistryTransportKind::Http,
                        endpoint: Some("http://127.0.0.1:8080".to_string()),
                        http_timeout_ms: 5000,
                        grpc_timeout_ms: 5000,
                        bridge_max_retries: 2,
                        bridge_initial_backoff_ms: 100,
                        bridge_max_backoff_ms: 1000,
                        bridge_circuit_breaker_failures: 5,
                        bridge_circuit_breaker_cooldown_ms: 3000,
                    },
                    session_id: "sess-old".to_string(),
                    registered_at: "2026-04-01T00:00:00Z".to_string(),
                    active_scope: Some("chat-001".to_string()),
                },
                registry::AppfsRegisteredAppDoc {
                    app_id: "stale".to_string(),
                    transport: registry::AppfsRegistryTransportDoc {
                        kind: registry::AppfsRegistryTransportKind::Grpc,
                        endpoint: Some("http://127.0.0.1:50051".to_string()),
                        http_timeout_ms: 5000,
                        grpc_timeout_ms: 5000,
                        bridge_max_retries: 2,
                        bridge_initial_backoff_ms: 100,
                        bridge_max_backoff_ms: 1000,
                        bridge_circuit_breaker_failures: 5,
                        bridge_circuit_breaker_cooldown_ms: 3000,
                    },
                    session_id: "sess-stale".to_string(),
                    registered_at: "2026-04-02T00:00:00Z".to_string(),
                    active_scope: Some("stale-scope".to_string()),
                },
            ],
        };

        let doc = build_registry_doc_from_resolved_apps(&resolved_apps, Some(&existing));

        assert_eq!(doc.apps.len(), 1);
        assert_eq!(doc.apps[0].app_id, "aiim");
        assert_eq!(doc.apps[0].session_id, "sess-aiim");
        assert_eq!(doc.apps[0].registered_at, "2026-04-01T00:00:00Z");
        assert_eq!(doc.apps[0].active_scope, None);
    }

    #[test]
    fn bootstrap_registry_from_resolved_apps_round_trips_into_runtime_args() {
        let temp = TempDir::new().expect("tempdir");
        let mut resolved_apps = BTreeMap::new();
        resolved_apps.insert(
            "aiim".to_string(),
            resolved_app(
                "aiim",
                "aiim-http",
                AppfsComposeTransportKind::Http,
                "http://127.0.0.1:8080",
                None,
            ),
        );
        resolved_apps.insert(
            "huoyan".to_string(),
            resolved_app(
                "huoyan",
                "huoyan-grpc",
                AppfsComposeTransportKind::Grpc,
                "http://127.0.0.1:50051",
                Some("sess-huoyan"),
            ),
        );

        let stored =
            bootstrap_registry_from_resolved_apps(temp.path(), &resolved_apps).expect("bootstrap");
        assert_eq!(stored.apps.len(), 2);
        assert_eq!(
            stored
                .apps
                .iter()
                .map(|app| app.app_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aiim", "huoyan"]
        );

        let runtime_args =
            registry::runtime_args_from_registry(&stored).expect("runtime args from registry");
        let resolved_runtime_args = resolve_runtime_cli_args(runtime_args);
        assert_eq!(resolved_runtime_args.len(), 2);
        assert_eq!(resolved_runtime_args[0].app_id, "aiim");
        assert_eq!(
            resolved_runtime_args[0]
                .bridge
                .adapter_http_endpoint
                .as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(resolved_runtime_args[1].app_id, "huoyan");
        assert_eq!(
            resolved_runtime_args[1]
                .bridge
                .adapter_grpc_endpoint
                .as_deref(),
            Some("http://127.0.0.1:50051")
        );
    }
}
