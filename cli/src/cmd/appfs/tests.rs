use serde_json::Value;

use super::action_dispatcher::{
    parse_action_line_v2, parse_paging_request, parse_snapshot_refresh_request,
    validate_submit_payload as validate_payload,
};
use super::errors::{ERR_INVALID_ARGUMENT, ERR_INVALID_PAYLOAD};
use super::shared::{
    action_template_matches, boundary_probe_from_bytes, decode_jsonl_line,
    deterministic_shorten_segment, extract_client_token, has_odd_unescaped_quotes,
    is_handle_format_valid, is_safe_resource_rel_path, normalize_resource_rel_path,
    normalize_runtime_handle_id, parse_snapshot_on_timeout_policy, recover_multiline_json_payload,
    template_specificity,
};
use super::{ActionSpec, ExecutionMode, InputMode, SnapshotOnTimeoutPolicy, MAX_SEGMENT_BYTES};

fn make_spec() -> ActionSpec {
    ActionSpec {
        template: "contacts/{contact_id}/send_message.act".to_string(),
        input_mode: InputMode::Json,
        execution_mode: ExecutionMode::Inline,
        max_payload_bytes: Some(8192),
    }
}

#[test]
fn parse_handle_json_mode() {
    let req = parse_paging_request(r#"{"handle_id":"ph_abc"}"#).expect("expected handle");
    assert_eq!(req.handle_id, "ph_abc");
    assert_eq!(req.session_id, None);
}

#[test]
fn parse_handle_json_with_session_mode() {
    let req = parse_paging_request(r#"{"handle_id":"ph_abc","session_id":"sess-other"}"#)
        .expect("expected handle");
    assert_eq!(req.handle_id, "ph_abc");
    assert_eq!(req.session_id.as_deref(), Some("sess-other"));
}

#[test]
fn extract_token_from_json() {
    let token = extract_client_token(r#"{"client_token":"x-1"}"#).expect("token missing");
    assert_eq!(token, "x-1");
}

#[test]
fn handle_format_validation() {
    assert!(is_handle_format_valid("ph_7f2c"));
    assert!(!is_handle_format_valid("bad/handle"));
}

#[test]
fn normalize_runtime_handle_id_keeps_short_value() {
    let handle = "ph_short_handle";
    assert_eq!(normalize_runtime_handle_id(handle), handle);
}

#[test]
fn deterministic_shorten_is_bounded_and_stable() {
    let long_handle = format!("ph_{}", "a".repeat(500));
    let shortened_a = deterministic_shorten_segment(&long_handle, MAX_SEGMENT_BYTES);
    let shortened_b = deterministic_shorten_segment(&long_handle, MAX_SEGMENT_BYTES);

    assert_eq!(shortened_a, shortened_b);
    assert!(shortened_a.starts_with("ph_"));
    assert!(shortened_a.as_bytes().len() <= MAX_SEGMENT_BYTES);
}

#[test]
fn json_payload_validation_accepts_object() {
    let spec = make_spec();
    let payload = r#"{"text":"hello line 1\nhello line 2"}"#;
    assert!(validate_payload(&spec, payload).is_ok());
}

#[test]
fn json_payload_validation_rejects_non_json() {
    let spec = make_spec();
    let payload = "hello line 1";
    assert!(validate_payload(&spec, payload).is_err());
}

#[test]
fn actionline_v2_parses_minimal_valid_line() {
    let parsed = parse_action_line_v2(
        r#"{"version":"2.0","client_token":"msg-001","payload":{"text":"hello"}}"#,
    )
    .expect("expected valid action line");
    assert_eq!(parsed.client_token, "msg-001");
    let payload: Value = serde_json::from_str(&parsed.payload_json).expect("json payload");
    assert_eq!(payload.get("text").and_then(|v| v.as_str()), Some("hello"));
}

#[test]
fn actionline_v2_supports_multiple_jsonl_lines() {
    let bytes = br#"{"version":"2.0","client_token":"msg-001","payload":{"text":"a"}}
{"version":"2.0","client_token":"msg-002","payload":{"text":"b"}}
"#;
    let mut parsed_tokens = Vec::new();
    for (idx, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut line_bytes = line.to_vec();
        line_bytes.push(b'\n');
        let decoded = decode_jsonl_line(&line_bytes, idx == 0)
            .expect("decode")
            .expect("line");
        let parsed = parse_action_line_v2(&decoded).expect("parse");
        parsed_tokens.push(parsed.client_token);
    }
    assert_eq!(parsed_tokens, vec!["msg-001", "msg-002"]);
}

#[test]
fn actionline_v2_rejects_raw_text() {
    let err = parse_action_line_v2("hello world").expect_err("raw text must be rejected");
    assert_eq!(err.code, ERR_INVALID_PAYLOAD);
}

#[test]
fn actionline_v2_rejects_non_object_json() {
    let err =
        parse_action_line_v2(r#"["not","object"]"#).expect_err("non-object json must be rejected");
    assert_eq!(err.code, ERR_INVALID_ARGUMENT);
}

#[test]
fn actionline_v2_rejects_mode_field() {
    let err = parse_action_line_v2(
        r#"{"version":"2.0","mode":"text","client_token":"x","payload":{"text":"hi"}}"#,
    )
    .expect_err("mode must be rejected");
    assert_eq!(err.code, ERR_INVALID_ARGUMENT);
}

#[test]
fn actionline_v2_rejects_missing_required_fields() {
    let missing_client = parse_action_line_v2(r#"{"version":"2.0","payload":{"text":"hi"}}"#)
        .expect_err("missing client_token");
    assert_eq!(missing_client.code, ERR_INVALID_ARGUMENT);

    let missing_payload = parse_action_line_v2(r#"{"version":"2.0","client_token":"x"}"#)
        .expect_err("missing payload");
    assert_eq!(missing_payload.code, ERR_INVALID_ARGUMENT);
}

#[test]
fn decode_jsonl_line_supports_utf8_bom_on_first_line() {
    let bytes = b"\xEF\xBB\xBF{\"text\":\"hello\"}\n";
    let line = decode_jsonl_line(bytes, true).expect("decode should succeed");
    assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
}

#[test]
fn decode_jsonl_line_trims_crlf() {
    let bytes = b"{\"text\":\"hello\"}\r\n";
    let line = decode_jsonl_line(bytes, false).expect("decode should succeed");
    assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
}

#[test]
fn decode_jsonl_line_rejects_invalid_utf8() {
    let bytes = [0xFF, 0xFF, b'\n'];
    assert!(decode_jsonl_line(&bytes, false).is_err());
}

#[test]
fn decode_jsonl_line_supports_utf16le_ps5_redirection() {
    let bytes = vec![
        0x7b, 0x00, 0x22, 0x00, 0x74, 0x00, 0x65, 0x00, 0x78, 0x00, 0x74, 0x00, 0x22, 0x00, 0x3a,
        0x00, 0x22, 0x00, 0x68, 0x00, 0x65, 0x00, 0x6c, 0x00, 0x6c, 0x00, 0x6f, 0x00, 0x22, 0x00,
        0x7d, 0x00, 0x0d, 0x00, 0x0a,
    ];
    let line = decode_jsonl_line(&bytes, false).expect("decode should succeed");
    assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
}

#[test]
fn decode_jsonl_line_supports_utf16le_bom() {
    let bytes = vec![
        0xff, 0xfe, 0x7b, 0x00, 0x22, 0x00, 0x6f, 0x00, 0x6b, 0x00, 0x22, 0x00, 0x3a, 0x00, 0x74,
        0x00, 0x72, 0x00, 0x75, 0x00, 0x65, 0x00, 0x7d, 0x00, 0x0a,
    ];
    let line = decode_jsonl_line(&bytes, true).expect("decode should succeed");
    assert_eq!(line.as_deref(), Some("{\"ok\":true}"));
}

#[test]
fn quote_parity_detects_shell_expanded_fragment() {
    assert!(has_odd_unescaped_quotes("{\"text\":\"hello"));
    assert!(!has_odd_unescaped_quotes("{\"text\":\"hello\\nworld\"}"));
}

#[test]
fn multiline_recovery_merges_three_lines_into_one_json() {
    let spec = make_spec();
    let bytes = b"{\"client_token\":\"ct-ml-1\",\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\nhello\n\xe5\xa5\xbd\xef\xbc\x81\"}\n";
    let first_line_end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .map(|idx| idx + 1)
        .expect("newline");
    let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
        .expect("decode")
        .expect("payload");

    let recovered = recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec)
        .expect("should recover");
    assert_eq!(recovered.2, 3);
    assert_eq!(recovered.1, bytes.len());

    let parsed: Value = serde_json::from_str(&recovered.0).expect("valid json");
    assert_eq!(
        parsed.get("text").and_then(|v| v.as_str()),
        Some("你好\nhello\n好！")
    );
}

#[test]
fn multiline_recovery_does_not_trigger_for_non_multiline_fragment() {
    let spec = make_spec();
    let bytes = b"{\"client_token\":\"ct-good\",\"text\":\"ok\"}\n{\"client_token\":\"ct-next\",\"text\":\"next\"}\n";
    let first_line_end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .map(|idx| idx + 1)
        .expect("newline");
    let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
        .expect("decode")
        .expect("payload");

    let recovered = recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec);
    assert!(recovered.is_none());
}

#[test]
fn multiline_recovery_stops_when_json_not_completed() {
    let spec = make_spec();
    let bytes = b"{\"client_token\":\"ct-bad\",\"text\":\"hello\nworld\n";
    let first_line_end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .map(|idx| idx + 1)
        .expect("newline");
    let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
        .expect("decode")
        .expect("payload");

    let recovered = recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec);
    assert!(recovered.is_none());
}
#[test]
fn parse_handle_rejects_non_json_payload() {
    assert!(parse_paging_request("ph_7f2c\n").is_err());
}

#[test]
fn parse_snapshot_refresh_requires_resource_path() {
    assert!(parse_snapshot_refresh_request(
        r#"{"resource_path":"/chats/chat-001/messages.res.jsonl"}"#
    )
    .is_ok());
    assert!(parse_snapshot_refresh_request(r#"{"path":"bad"}"#).is_err());
}

#[test]
fn snapshot_on_timeout_policy_defaults_to_return_stale() {
    assert_eq!(
        parse_snapshot_on_timeout_policy(None),
        SnapshotOnTimeoutPolicy::ReturnStale
    );
    assert_eq!(
        parse_snapshot_on_timeout_policy(Some("")),
        SnapshotOnTimeoutPolicy::ReturnStale
    );
    assert_eq!(
        parse_snapshot_on_timeout_policy(Some("return_stale")),
        SnapshotOnTimeoutPolicy::ReturnStale
    );
}

#[test]
fn snapshot_on_timeout_policy_parses_fail() {
    assert_eq!(
        parse_snapshot_on_timeout_policy(Some("fail")),
        SnapshotOnTimeoutPolicy::Fail
    );
    assert_eq!(
        parse_snapshot_on_timeout_policy(Some("FAIL")),
        SnapshotOnTimeoutPolicy::Fail
    );
}

#[test]
fn resource_path_normalization_and_safety() {
    assert_eq!(
        normalize_resource_rel_path("/chats/chat-001/messages.res.jsonl").as_deref(),
        Some("chats/chat-001/messages.res.jsonl")
    );
    assert!(is_safe_resource_rel_path(
        "chats/chat-001/messages.res.jsonl"
    ));
    assert!(!is_safe_resource_rel_path("../etc/passwd"));
    assert!(!is_safe_resource_rel_path(
        "chats/chat-001/messages.res.json"
    ));
}

#[test]
fn template_specificity_prefers_concrete_snapshot_template() {
    let rel = "chats/chat-oversize/messages.res.jsonl";
    let generic = "chats/{chat_id}/messages.res.jsonl";
    let concrete = "chats/chat-oversize/messages.res.jsonl";
    assert!(action_template_matches(generic, rel));
    assert!(action_template_matches(concrete, rel));

    let selected = [generic, concrete]
        .into_iter()
        .filter(|template| action_template_matches(template, rel))
        .max_by_key(|template| template_specificity(template))
        .expect("expected at least one template match");
    assert_eq!(selected, concrete);
}

#[test]
fn boundary_probe_is_stable() {
    let bytes = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let probe_a = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
    let probe_b = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
    assert_eq!(probe_a, probe_b);
}
