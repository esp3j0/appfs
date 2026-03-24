use anyhow::{Context, Result};
use serde_json::Value as JsonValue;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{
    ActionSpec, SnapshotOnTimeoutPolicy, ACTION_CURSOR_PROBE_WINDOW, ALLOWED_SEGMENT_CHARS,
    DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS, MAX_RECOVERY_BYTES, MAX_RECOVERY_LINES, MAX_SEGMENT_BYTES,
    SNAPSHOT_COALESCE_WINDOW_ENV, SNAPSHOT_EXPAND_DELAY_ENV, SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV,
    SNAPSHOT_PUBLISH_DELAY_ENV,
};

pub(super) fn collect_files_with_suffix(
    dir: &Path,
    suffix: &str,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_with_suffix(&path, suffix, out)?;
            continue;
        }

        if file_type.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.ends_with(suffix))
        {
            out.push(path);
        }
    }
    Ok(())
}

pub(super) fn boundary_probe_from_bytes(bytes: &[u8], offset: u64) -> Option<String> {
    if offset == 0 {
        return None;
    }
    let end = offset.min(bytes.len() as u64) as usize;
    if end == 0 {
        return None;
    }
    let start = end.saturating_sub(ACTION_CURSOR_PROBE_WINDOW);
    let hash = fnv1a_64(&bytes[start..end]);
    Some(format!("{hash:016x}"))
}

pub(super) fn decode_jsonl_line(
    line_bytes: &[u8],
    allow_bom: bool,
) -> std::result::Result<Option<String>, String> {
    if line_bytes.is_empty() {
        return Ok(None);
    }

    let mut slice = line_bytes;
    if allow_bom && slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
        slice = &slice[3..];
    }
    if slice.ends_with(b"\n") {
        slice = &slice[..slice.len().saturating_sub(1)];
    }
    if slice.ends_with(b"\r") {
        slice = &slice[..slice.len().saturating_sub(1)];
    }
    if slice.is_empty() {
        return Ok(None);
    }

    if let Some(decoded_utf16) = try_decode_utf16_line(slice, allow_bom) {
        if decoded_utf16.is_empty() {
            return Ok(None);
        }
        return Ok(Some(decoded_utf16));
    }

    let decoded = std::str::from_utf8(slice)
        .map_err(|err| format!("utf8 decode failed for JSONL line: {err}"))?;
    Ok(Some(decoded.to_string()))
}

pub(super) fn has_odd_unescaped_quotes(s: &str) -> bool {
    let mut escaped = false;
    let mut quote_count = 0usize;
    for ch in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quote_count += 1;
        }
    }
    quote_count % 2 == 1
}

pub(super) enum MultilineRecoveryOutcome {
    Recovered {
        merged_payload: String,
        merged_line_end: usize,
        consumed_lines: usize,
    },
    PendingAtEof,
}

pub(super) fn classify_multiline_json_payload(
    bytes: &[u8],
    initial_payload: &str,
    initial_line_end: usize,
    spec: &ActionSpec,
) -> Option<MultilineRecoveryOutcome> {
    if !has_odd_unescaped_quotes(initial_payload) {
        return None;
    }

    let max_recovery_bytes = spec
        .max_payload_bytes
        .unwrap_or(MAX_RECOVERY_BYTES)
        .min(MAX_RECOVERY_BYTES);
    if initial_payload.len() >= max_recovery_bytes {
        return None;
    }

    let mut merged = initial_payload.to_string();
    let mut consumed_lines = 1usize;
    let mut next_position = initial_line_end;
    let mut exhausted_eof = next_position >= bytes.len();
    let mut saw_standalone_json_fragment = false;

    while consumed_lines < MAX_RECOVERY_LINES {
        while next_position < bytes.len() && bytes[next_position] == 0 {
            next_position += 1;
        }
        if next_position >= bytes.len() {
            exhausted_eof = true;
            break;
        }

        let Some(next_rel_idx) = bytes[next_position..].iter().position(|b| *b == b'\n') else {
            exhausted_eof = true;
            break;
        };
        let next_end = next_position + next_rel_idx + 1;
        let next_line_bytes = &bytes[next_position..next_end];
        let next_fragment = match decode_jsonl_line(next_line_bytes, next_position == 0) {
            Ok(Some(value)) => value,
            Ok(None) => String::new(),
            Err(_) => break,
        };
        if serde_json::from_str::<JsonValue>(&next_fragment).is_ok() {
            saw_standalone_json_fragment = true;
        }

        let candidate_len = merged
            .len()
            .saturating_add(2)
            .saturating_add(next_fragment.len());
        if candidate_len > max_recovery_bytes {
            break;
        }

        merged.push_str("\\n");
        merged.push_str(&next_fragment);
        consumed_lines += 1;

        if serde_json::from_str::<JsonValue>(&merged).is_ok() {
            return Some(MultilineRecoveryOutcome::Recovered {
                merged_payload: merged,
                merged_line_end: next_end,
                consumed_lines,
            });
        }
        next_position = next_end;
    }

    if exhausted_eof && !saw_standalone_json_fragment {
        return Some(MultilineRecoveryOutcome::PendingAtEof);
    }

    None
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn recover_multiline_json_payload(
    bytes: &[u8],
    initial_payload: &str,
    initial_line_end: usize,
    spec: &ActionSpec,
) -> Option<(String, usize, usize)> {
    match classify_multiline_json_payload(bytes, initial_payload, initial_line_end, spec) {
        Some(MultilineRecoveryOutcome::Recovered {
            merged_payload,
            merged_line_end,
            consumed_lines,
        }) => Some((merged_payload, merged_line_end, consumed_lines)),
        _ => None,
    }
}
#[derive(Clone, Copy)]
enum Utf16Endian {
    Le,
    Be,
}

fn try_decode_utf16_line(slice: &[u8], allow_bom: bool) -> Option<String> {
    if !slice.contains(&0x00) {
        return None;
    }

    let mut bytes = slice;
    let mut endian: Option<Utf16Endian> = None;

    if allow_bom && bytes.starts_with(&[0xFF, 0xFE]) {
        bytes = &bytes[2..];
        endian = Some(Utf16Endian::Le);
    } else if allow_bom && bytes.starts_with(&[0xFE, 0xFF]) {
        bytes = &bytes[2..];
        endian = Some(Utf16Endian::Be);
    }

    if bytes.is_empty() {
        return Some(String::new());
    }

    if !bytes.len().is_multiple_of(2) {
        return None;
    }

    if endian.is_none() {
        let pair_count = bytes.len() / 2;
        if pair_count == 0 {
            return None;
        }
        let odd_zeros = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        let even_zeros = bytes.iter().step_by(2).filter(|b| **b == 0).count();

        if odd_zeros * 2 >= pair_count {
            endian = Some(Utf16Endian::Le);
        } else if even_zeros * 2 >= pair_count {
            endian = Some(Utf16Endian::Be);
        } else {
            return None;
        }
    }

    let mut units = Vec::with_capacity(bytes.len() / 2);
    match endian.expect("utf16 endianness should be detected") {
        Utf16Endian::Le => {
            for pair in bytes.chunks_exact(2) {
                units.push(u16::from_le_bytes([pair[0], pair[1]]));
            }
        }
        Utf16Endian::Be => {
            for pair in bytes.chunks_exact(2) {
                units.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
        }
    }

    while matches!(units.last(), Some(0x000d | 0x000a)) {
        units.pop();
    }
    if units.is_empty() {
        return Some(String::new());
    }

    let mut out = String::with_capacity(units.len());
    for ch in std::char::decode_utf16(units) {
        let ch = ch.ok()?;
        out.push(ch);
    }
    Some(out)
}

pub(super) fn is_transient_action_sink_busy(err: &std::io::Error) -> bool {
    if !matches!(
        err.kind(),
        ErrorKind::PermissionDenied | ErrorKind::WouldBlock
    ) {
        return false;
    }

    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(5 | 32 | 33))
    }

    #[cfg(not(windows))]
    {
        false
    }
}

pub(super) fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(super) fn snapshot_expand_delay_ms() -> u64 {
    std::env::var(SNAPSHOT_EXPAND_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

pub(super) fn snapshot_publish_delay_ms() -> u64 {
    std::env::var(SNAPSHOT_PUBLISH_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

pub(super) fn snapshot_force_expand_on_refresh() -> bool {
    env_flag_enabled(SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV)
}

pub(super) fn snapshot_coalesce_window_ms() -> u64 {
    std::env::var(SNAPSHOT_COALESCE_WINDOW_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS)
}

pub(super) fn parse_snapshot_on_timeout_policy(value: Option<&str>) -> SnapshotOnTimeoutPolicy {
    let normalized = value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    match normalized.as_deref() {
        Some("fail") => SnapshotOnTimeoutPolicy::Fail,
        Some("return_stale") | None => SnapshotOnTimeoutPolicy::ReturnStale,
        Some(other) => {
            eprintln!(
                "AppFS adapter unknown snapshot.on_timeout='{other}', defaulting to return_stale"
            );
            SnapshotOnTimeoutPolicy::ReturnStale
        }
    }
}

pub(super) fn action_template_matches(template: &str, rel_path: &str) -> bool {
    let template = template.trim_matches('/');
    let rel_path = rel_path.trim_matches('/');
    if template.is_empty() || rel_path.is_empty() {
        return false;
    }

    let template_segments: Vec<&str> = template.split('/').collect();
    let rel_segments: Vec<&str> = rel_path.split('/').collect();
    if template_segments.len() != rel_segments.len() {
        return false;
    }

    template_segments
        .iter()
        .zip(rel_segments.iter())
        .all(|(t, r)| {
            if is_template_placeholder(t) {
                !r.is_empty()
            } else {
                *t == *r
            }
        })
}

/// Specificity score for template matching.
///
/// Higher is more specific: prefer templates with more literal segments/bytes
/// over placeholder-heavy templates (for example, prefer
/// `chats/chat-oversize/messages.res.jsonl` over `chats/{chat_id}/messages.res.jsonl`).
pub(super) fn template_specificity(template: &str) -> (usize, usize, usize) {
    let mut literal_segments = 0usize;
    let mut literal_bytes = 0usize;
    let mut total_segments = 0usize;
    for segment in template.trim_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        total_segments += 1;
        if !is_template_placeholder(segment) {
            literal_segments += 1;
            literal_bytes += segment.len();
        }
    }
    (literal_segments, literal_bytes, total_segments)
}

fn is_template_placeholder(segment: &str) -> bool {
    segment.len() >= 3 && segment.starts_with('{') && segment.ends_with('}')
}

pub(super) fn is_safe_action_rel_path(rel_path: &str) -> bool {
    let path = rel_path.trim_matches('/');
    if path.is_empty() {
        return false;
    }

    path.ends_with(".act") && path.split('/').all(is_safe_segment)
}

pub(super) fn is_safe_resource_rel_path(rel_path: &str) -> bool {
    let path = rel_path.trim_matches('/');
    if path.is_empty() {
        return false;
    }
    if !path.ends_with(".res.jsonl") {
        return false;
    }

    path.split('/').all(is_safe_segment)
}

fn is_safe_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." {
        return false;
    }
    if segment.contains('\\') || segment.contains('\0') {
        return false;
    }
    if is_drive_letter_segment(segment) {
        return false;
    }
    if is_windows_reserved_name(segment) {
        return false;
    }
    if segment.len() > MAX_SEGMENT_BYTES {
        return false;
    }

    segment.chars().all(|c| ALLOWED_SEGMENT_CHARS.contains(c))
}

fn is_drive_letter_segment(segment: &str) -> bool {
    segment.len() >= 2
        && segment.as_bytes()[0].is_ascii_alphabetic()
        && segment.as_bytes()[1] == b':'
}

fn is_windows_reserved_name(segment: &str) -> bool {
    let upper = segment.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub(super) fn extract_client_token(payload: &str) -> Option<String> {
    let json = serde_json::from_str::<JsonValue>(payload).ok()?;
    json.get("client_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

pub(super) fn normalize_resource_rel_path(path: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized)
}

pub(super) fn parse_rfc3339_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp())
}

pub(super) fn normalize_runtime_handle_id(handle_id: &str) -> String {
    deterministic_shorten_segment(handle_id, MAX_SEGMENT_BYTES)
}

pub(super) fn deterministic_shorten_segment(segment: &str, max_bytes: usize) -> String {
    if segment.len() <= max_bytes {
        return segment.to_string();
    }

    let hash = format!("{:016x}", fnv1a_64(segment.as_bytes()));
    let suffix = format!("_{}", hash);
    let prefix_budget = max_bytes.saturating_sub(suffix.len());

    let mut prefix = String::new();
    let mut used = 0usize;
    for ch in segment.chars() {
        let ch_len = ch.len_utf8();
        if used + ch_len > prefix_budget {
            break;
        }
        prefix.push(ch);
        used += ch_len;
    }

    if prefix.is_empty() {
        return hash;
    }

    prefix.push_str(&suffix);
    prefix
}

fn fnv1a_64(input: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET_BASIS;
    for byte in input {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub(super) fn is_handle_format_valid(handle_id: &str) -> bool {
    if !handle_id.starts_with("ph_") {
        return false;
    }
    handle_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}
