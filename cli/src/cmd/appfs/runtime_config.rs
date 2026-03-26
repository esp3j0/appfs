use super::bridge_resilience::BridgeRuntimeOptions;
use anyhow::Result;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct AppfsBridgeCliArgs {
    pub adapter_http_endpoint: Option<String>,
    pub adapter_http_timeout_ms: u64,
    pub adapter_grpc_endpoint: Option<String>,
    pub adapter_grpc_timeout_ms: u64,
    pub adapter_bridge_max_retries: u32,
    pub adapter_bridge_initial_backoff_ms: u64,
    pub adapter_bridge_max_backoff_ms: u64,
    pub adapter_bridge_circuit_breaker_failures: u32,
    pub adapter_bridge_circuit_breaker_cooldown_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAppfsRuntimeCliArgs {
    pub app_id: String,
    pub session_id: String,
    pub bridge: AppfsBridgeCliArgs,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct AppfsRuntimeCliArgs {
    pub app_id: String,
    pub session_id: Option<String>,
    pub bridge: AppfsBridgeCliArgs,
}

#[derive(Debug, Clone)]
pub(crate) struct AppfsBridgeConfig {
    pub(super) adapter_http_endpoint: Option<String>,
    pub(super) adapter_http_timeout_ms: u64,
    pub(super) adapter_grpc_endpoint: Option<String>,
    pub(super) adapter_grpc_timeout_ms: u64,
    pub(super) runtime_options: BridgeRuntimeOptions,
}

pub(crate) fn normalize_appfs_app_ids(
    primary_app_id: Option<String>,
    extra_app_ids: Vec<String>,
    default_app_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut seen = std::collections::HashMap::new();
    let mut ordered = Vec::new();

    fn push_unique_app_id(
        seen: &mut std::collections::HashMap<String, ()>,
        ordered: &mut Vec<String>,
        raw: String,
    ) -> Result<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("app id cannot be empty");
        }
        if seen.insert(trimmed.to_string(), ()).is_none() {
            ordered.push(trimmed.to_string());
        }
        Ok(())
    }

    if let Some(primary) = primary_app_id {
        push_unique_app_id(&mut seen, &mut ordered, primary)?;
    }
    for app_id in extra_app_ids {
        push_unique_app_id(&mut seen, &mut ordered, app_id)?;
    }

    if ordered.is_empty() {
        if let Some(default_app_id) = default_app_id {
            push_unique_app_id(&mut seen, &mut ordered, default_app_id.to_string())?;
        }
    }

    Ok(ordered)
}

pub(crate) fn build_runtime_cli_args(
    primary_app_id: Option<String>,
    extra_app_ids: Vec<String>,
    session_id: Option<String>,
    bridge: AppfsBridgeCliArgs,
    default_app_id: Option<&str>,
) -> Result<Vec<AppfsRuntimeCliArgs>> {
    let app_ids = normalize_appfs_app_ids(primary_app_id, extra_app_ids, default_app_id)?;
    if app_ids.len() > 1 && session_id.is_some() {
        anyhow::bail!(
            "multi-app AppFS runtime does not accept a single shared --session-id; omit it and runtime will generate isolated per-app sessions"
        );
    }

    Ok(app_ids
        .into_iter()
        .map(|app_id| AppfsRuntimeCliArgs {
            app_id,
            session_id: session_id.clone(),
            bridge: bridge.clone(),
        })
        .collect())
}

pub(crate) fn normalize_appfs_session_id(session_id: Option<String>) -> String {
    session_id.unwrap_or_else(|| {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("sess-{}", &uuid[..8])
    })
}

pub(crate) fn resolve_runtime_cli_args(
    runtime_args: Vec<AppfsRuntimeCliArgs>,
) -> Vec<ResolvedAppfsRuntimeCliArgs> {
    runtime_args
        .into_iter()
        .map(|runtime| ResolvedAppfsRuntimeCliArgs {
            app_id: runtime.app_id,
            session_id: normalize_appfs_session_id(runtime.session_id),
            bridge: runtime.bridge,
        })
        .collect()
}

pub(crate) fn build_appfs_bridge_config(args: AppfsBridgeCliArgs) -> AppfsBridgeConfig {
    let bridge_runtime_options = BridgeRuntimeOptions::from_cli(
        args.adapter_bridge_max_retries,
        args.adapter_bridge_initial_backoff_ms,
        args.adapter_bridge_max_backoff_ms,
        args.adapter_bridge_circuit_breaker_failures,
        args.adapter_bridge_circuit_breaker_cooldown_ms,
    );
    AppfsBridgeConfig {
        adapter_http_endpoint: args.adapter_http_endpoint,
        adapter_http_timeout_ms: args.adapter_http_timeout_ms,
        adapter_grpc_endpoint: args.adapter_grpc_endpoint,
        adapter_grpc_timeout_ms: args.adapter_grpc_timeout_ms,
        runtime_options: bridge_runtime_options,
    }
}
