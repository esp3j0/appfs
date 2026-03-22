use serde_json::Value as JsonValue;

use super::{ActionSpec, InputMode, ERR_INVALID_ARGUMENT, ERR_INVALID_PAYLOAD};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedActionLineV2 {
    pub(super) client_token: String,
    pub(super) payload_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionLineV2ValidationError {
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
pub(super) enum DispatchRoute {
    PagingFetchNext(PagingRequest),
    PagingClose(PagingRequest),
    SnapshotRefresh(SnapshotRefreshRequest),
    BusinessSubmit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DispatchRouteParseError {
    MalformedPagingFetchNext,
    MalformedPagingClose,
    MalformedSnapshotRefresh,
}

pub(super) fn normalize_actionline_v2_payload(
    payload: &str,
    strict: bool,
) -> std::result::Result<Option<ParsedActionLineV2>, ActionLineV2ValidationError> {
    if !strict {
        return Ok(None);
    }
    parse_action_line_v2(payload).map(Some)
}

pub(super) fn route_action(
    normalized_path: &str,
    payload: &str,
) -> std::result::Result<DispatchRoute, DispatchRouteParseError> {
    if normalized_path == "/_paging/fetch_next.act" {
        return parse_paging_request(payload)
            .map(DispatchRoute::PagingFetchNext)
            .map_err(|_| DispatchRouteParseError::MalformedPagingFetchNext);
    }
    if normalized_path == "/_paging/close.act" {
        return parse_paging_request(payload)
            .map(DispatchRoute::PagingClose)
            .map_err(|_| DispatchRouteParseError::MalformedPagingClose);
    }
    if normalized_path == "/_snapshot/refresh.act" {
        return parse_snapshot_refresh_request(payload)
            .map(DispatchRoute::SnapshotRefresh)
            .map_err(|_| DispatchRouteParseError::MalformedSnapshotRefresh);
    }

    Ok(DispatchRoute::BusinessSubmit)
}

pub(super) fn parse_action_line_v2(
    line: &str,
) -> std::result::Result<ParsedActionLineV2, ActionLineV2ValidationError> {
    let json =
        serde_json::from_str::<JsonValue>(line).map_err(|_| ActionLineV2ValidationError {
            code: ERR_INVALID_PAYLOAD,
            reason: "action line must be valid json",
        })?;

    let object = json.as_object().ok_or(ActionLineV2ValidationError {
        code: ERR_INVALID_ARGUMENT,
        reason: "action line must be a json object",
    })?;

    if object.contains_key("mode") {
        return Err(ActionLineV2ValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "mode field is not allowed in ActionLineV2",
        });
    }

    let version = object
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or(ActionLineV2ValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "version is required",
        })?;

    if version != "2.0" {
        return Err(ActionLineV2ValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "version must be 2.0",
        });
    }

    let client_token = object
        .get("client_token")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ActionLineV2ValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "client_token is required",
        })?
        .to_string();

    let payload = object
        .get("payload")
        .and_then(|value| value.as_object())
        .ok_or(ActionLineV2ValidationError {
            code: ERR_INVALID_ARGUMENT,
            reason: "payload must be a json object",
        })?;

    let payload_json = serde_json::to_string(payload).map_err(|_| ActionLineV2ValidationError {
        code: ERR_INVALID_PAYLOAD,
        reason: "payload serialization failed",
    })?;

    Ok(ParsedActionLineV2 {
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
