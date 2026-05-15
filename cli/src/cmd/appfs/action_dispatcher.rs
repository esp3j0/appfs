use serde_json::Value as JsonValue;

use super::errors::{ERR_INVALID_ARGUMENT, ERR_INVALID_PAYLOAD};
use super::registry::AppfsRegistryTransportDoc;
use super::{ActionSpec, InputMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedActionLine {
    pub(super) client_token: String,
    pub(super) payload_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionLineValidationError {
    pub(super) code: &'static str,
    pub(super) reason: &'static str,
}

#[derive(Debug, Clone)]
pub(super) struct PagingRequest {
    pub(super) handle_id: String,
    pub(super) session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SnapshotRefreshRequest {
    pub(super) resource_path: String,
}

#[derive(Debug, Clone)]
pub(super) struct EnterScopeRequest {
    pub(super) target_scope: String,
}

#[derive(Debug, Clone)]
pub(super) struct StructureRefreshRequest {
    pub(super) target_scope: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct EnsureCredentialsRequest {
    pub(super) expected_profile_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct RegisterAppRequest {
    pub(super) app_id: String,
    pub(super) session_id: Option<String>,
    pub(super) transport: AppfsRegistryTransportDoc,
}

#[derive(Debug, Clone)]
pub(super) struct UnregisterAppRequest {
    pub(super) app_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct CreatePrincipalRequest {
    pub(super) principal_id: String,
    pub(super) display_name: String,
    pub(super) description: Option<String>,
    pub(super) kind: String,
}

#[derive(Debug, Clone)]
pub(super) struct UpdatePrincipalRequest {
    pub(super) principal_id: String,
    pub(super) display_name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) kind: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct DeletePrincipalRequest {
    pub(super) principal_id: String,
}

#[derive(Debug, Clone)]
pub(super) enum DispatchRoute {
    PagingFetchNext(PagingRequest),
    PagingClose(PagingRequest),
    SnapshotRefresh(SnapshotRefreshRequest),
    EnterScope(EnterScopeRequest),
    StructureRefresh(StructureRefreshRequest),
    EnsureCredentials(EnsureCredentialsRequest),
    BusinessSubmit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DispatchRouteParseError {
    PagingFetchNext,
    PagingClose,
    SnapshotRefresh,
    EnterScope,
    StructureRefresh,
    EnsureCredentials,
}

pub(super) fn normalize_actionline_payload(
    payload: &str,
    strict: bool,
) -> std::result::Result<Option<ParsedActionLine>, ActionLineValidationError> {
    if !strict {
        return Ok(None);
    }
    parse_action_line(payload).map(Some)
}

pub(super) fn route_action(
    normalized_path: &str,
    payload: &str,
) -> std::result::Result<DispatchRoute, DispatchRouteParseError> {
    if normalized_path == "/_paging/fetch_next.act" {
        return parse_paging_request(payload)
            .map(DispatchRoute::PagingFetchNext)
            .map_err(|_| DispatchRouteParseError::PagingFetchNext);
    }
    if normalized_path == "/_paging/close.act" {
        return parse_paging_request(payload)
            .map(DispatchRoute::PagingClose)
            .map_err(|_| DispatchRouteParseError::PagingClose);
    }
    if normalized_path == "/_snapshot/refresh.act" {
        return parse_snapshot_refresh_request(payload)
            .map(DispatchRoute::SnapshotRefresh)
            .map_err(|_| DispatchRouteParseError::SnapshotRefresh);
    }
    if normalized_path == "/_app/enter_scope.act" {
        return parse_enter_scope_request(payload)
            .map(DispatchRoute::EnterScope)
            .map_err(|_| DispatchRouteParseError::EnterScope);
    }
    if normalized_path == "/_app/refresh_structure.act" {
        return parse_structure_refresh_request(payload)
            .map(DispatchRoute::StructureRefresh)
            .map_err(|_| DispatchRouteParseError::StructureRefresh);
    }
    if normalized_path == "/_app/ensure_credentials.act" {
        return parse_ensure_credentials_request(payload)
            .map(DispatchRoute::EnsureCredentials)
            .map_err(|_| DispatchRouteParseError::EnsureCredentials);
    }

    Ok(DispatchRoute::BusinessSubmit)
}

pub(super) fn parse_action_line(
    line: &str,
) -> std::result::Result<ParsedActionLine, ActionLineValidationError> {
    let json = serde_json::from_str::<JsonValue>(line).map_err(|_| ActionLineValidationError {
        code: ERR_INVALID_PAYLOAD,
        reason: "action line must be valid json",
    })?;

    let object = json.as_object().ok_or(ActionLineValidationError {
        code: ERR_INVALID_ARGUMENT,
        reason: "action line must be a json object",
    })?;

    if object.contains_key("mode") {
        return Err(ActionLineValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "mode field is not allowed in ActionLine",
        });
    }

    let version = object
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or(ActionLineValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "version is required",
        })?;

    if version != "2.0" {
        return Err(ActionLineValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "version must be 2.0",
        });
    }

    let client_token = object
        .get("client_token")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ActionLineValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "client_token is required",
        })?
        .to_string();

    let payload = object
        .get("payload")
        .and_then(|value| value.as_object())
        .ok_or(ActionLineValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "payload must be a json object",
        })?;

    let payload_json = serde_json::to_string(payload).map_err(|_| ActionLineValidationError {
        code: ERR_INVALID_PAYLOAD,
        reason: "payload serialization failed",
    })?;

    Ok(ParsedActionLine {
        client_token,
        payload_json,
    })
}

pub(super) fn validate_submit_payload(
    spec: &ActionSpec,
    payload: &str,
) -> std::result::Result<(), &'static str> {
    if let Some(max) = spec.max_payload_bytes {
        if payload.len() > max {
            return Err("EMSGSIZE");
        }
    }

    if payload.trim().is_empty() {
        return Err(ERR_INVALID_ARGUMENT);
    }

    match spec.input_mode {
        InputMode::Json => {
            serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_PAYLOAD)?;
            Ok(())
        }
    }
}

pub(super) fn parse_paging_request(
    payload: &str,
) -> std::result::Result<PagingRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let handle_id = json
        .get("handle_id")
        .and_then(|v| v.as_str())
        .ok_or(ERR_INVALID_ARGUMENT)?;
    let session_id = json
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    Ok(PagingRequest {
        handle_id: handle_id.trim().to_string(),
        session_id,
    })
}

pub(super) fn parse_snapshot_refresh_request(
    payload: &str,
) -> std::result::Result<SnapshotRefreshRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let resource_path = json
        .get("resource_path")
        .and_then(|v| v.as_str())
        .ok_or(ERR_INVALID_ARGUMENT)?;
    Ok(SnapshotRefreshRequest {
        resource_path: resource_path.trim().to_string(),
    })
}

pub(super) fn parse_enter_scope_request(
    payload: &str,
) -> std::result::Result<EnterScopeRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let target_scope = json
        .get("target_scope")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(ERR_INVALID_ARGUMENT)?;
    Ok(EnterScopeRequest {
        target_scope: target_scope.to_string(),
    })
}

pub(super) fn parse_structure_refresh_request(
    payload: &str,
) -> std::result::Result<StructureRefreshRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let target_scope = json
        .get("target_scope")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    Ok(StructureRefreshRequest { target_scope })
}

pub(super) fn parse_ensure_credentials_request(
    payload: &str,
) -> std::result::Result<EnsureCredentialsRequest, &'static str> {
    let object = parse_json_object(payload)?;
    Ok(EnsureCredentialsRequest {
        expected_profile_id: optional_string(&object, "expected_profile_id")?,
    })
}

pub(super) fn parse_register_app_request(
    payload: &str,
) -> std::result::Result<RegisterAppRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let object = json.as_object().ok_or(ERR_INVALID_ARGUMENT)?;
    let app_id = object
        .get("app_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ERR_INVALID_ARGUMENT)?
        .to_string();
    let session_id = object
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let transport = object
        .get("transport")
        .cloned()
        .ok_or(ERR_INVALID_ARGUMENT)
        .and_then(|value| {
            serde_json::from_value::<AppfsRegistryTransportDoc>(value)
                .map_err(|_| ERR_INVALID_ARGUMENT)
        })?;
    Ok(RegisterAppRequest {
        app_id,
        session_id,
        transport,
    })
}

pub(super) fn parse_unregister_app_request(
    payload: &str,
) -> std::result::Result<UnregisterAppRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let app_id = json
        .get("app_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ERR_INVALID_ARGUMENT)?
        .to_string();
    Ok(UnregisterAppRequest { app_id })
}

pub(super) fn parse_list_apps_request(payload: &str) -> std::result::Result<(), &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    if json.is_object() {
        Ok(())
    } else {
        Err(ERR_INVALID_ARGUMENT)
    }
}

pub(super) fn parse_create_principal_request(
    payload: &str,
) -> std::result::Result<CreatePrincipalRequest, &'static str> {
    let object = parse_json_object(payload)?;
    let principal_id = required_principal_id(&object, "principal_id")?;
    let display_name = required_string(&object, "display_name")?;
    let description = optional_string(&object, "description")?;
    let kind = optional_string(&object, "kind")?.unwrap_or_else(|| "agent".to_string());
    Ok(CreatePrincipalRequest {
        principal_id,
        display_name,
        description,
        kind,
    })
}

pub(super) fn parse_update_principal_request(
    payload: &str,
) -> std::result::Result<UpdatePrincipalRequest, &'static str> {
    let object = parse_json_object(payload)?;
    let principal_id = required_principal_id(&object, "principal_id")?;
    Ok(UpdatePrincipalRequest {
        principal_id,
        display_name: optional_string(&object, "display_name")?,
        description: optional_string(&object, "description")?,
        kind: optional_string(&object, "kind")?,
    })
}

pub(super) fn parse_delete_principal_request(
    payload: &str,
) -> std::result::Result<DeletePrincipalRequest, &'static str> {
    let object = parse_json_object(payload)?;
    let principal_id = required_principal_id(&object, "principal_id")?;
    Ok(DeletePrincipalRequest { principal_id })
}

fn parse_json_object(
    payload: &str,
) -> std::result::Result<serde_json::Map<String, JsonValue>, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    json.as_object().cloned().ok_or(ERR_INVALID_ARGUMENT)
}

fn required_string(
    object: &serde_json::Map<String, JsonValue>,
    field_name: &str,
) -> std::result::Result<String, &'static str> {
    object
        .get(field_name)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(ERR_INVALID_ARGUMENT)
}

fn optional_string(
    object: &serde_json::Map<String, JsonValue>,
    field_name: &str,
) -> std::result::Result<Option<String>, &'static str> {
    object
        .get(field_name)
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or(ERR_INVALID_ARGUMENT)
        })
        .transpose()
}

fn required_principal_id(
    object: &serde_json::Map<String, JsonValue>,
    field_name: &str,
) -> std::result::Result<String, &'static str> {
    let principal_id = required_string(object, field_name)?;
    if is_safe_principal_id(&principal_id) {
        Ok(principal_id)
    } else {
        Err(ERR_INVALID_ARGUMENT)
    }
}

fn is_safe_principal_id(value: &str) -> bool {
    if value == "." || value == ".." || value.len() > 120 {
        return false;
    }
    if value.contains(['/', '\\', '\0', ':']) {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}
