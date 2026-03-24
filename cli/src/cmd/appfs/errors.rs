pub(super) const ERR_PAGER_HANDLE_NOT_FOUND: &str = "PAGER_HANDLE_NOT_FOUND";
pub(super) const ERR_PAGER_HANDLE_EXPIRED: &str = "PAGER_HANDLE_EXPIRED";
pub(super) const ERR_PAGER_HANDLE_CLOSED: &str = "PAGER_HANDLE_CLOSED";
pub(super) const ERR_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub(super) const ERR_INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
pub(super) const ERR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub(super) const ERR_SNAPSHOT_TOO_LARGE: &str = "SNAPSHOT_TOO_LARGE";
pub(super) const ERR_CACHE_MISS_EXPAND_FAILED: &str = "CACHE_MISS_EXPAND_FAILED";

pub(super) fn is_transient_connector_failure(code: &str, retryable: bool) -> bool {
    if !retryable {
        return false;
    }

    code.eq_ignore_ascii_case("INTERNAL")
        || code.eq_ignore_ascii_case("UPSTREAM_UNAVAILABLE")
        || code.eq_ignore_ascii_case("TIMEOUT")
}

#[cfg(test)]
mod tests {
    use super::is_transient_connector_failure;

    #[test]
    fn transient_connector_failure_matrix() {
        assert!(is_transient_connector_failure("INTERNAL", true));
        assert!(is_transient_connector_failure("UPSTREAM_UNAVAILABLE", true));
        assert!(is_transient_connector_failure("TIMEOUT", true));

        assert!(!is_transient_connector_failure("INTERNAL", false));
        assert!(!is_transient_connector_failure("INVALID_PAYLOAD", true));
        assert!(!is_transient_connector_failure("PERMISSION_DENIED", true));
    }
}
