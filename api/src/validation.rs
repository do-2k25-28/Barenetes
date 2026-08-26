// Shared identifier validation, so every handler that accepts a pod/namespace
// name enforces the same rule instead of duplicating it.
use tonic::Status;

/// Validates a DNS-1123-subdomain-style identifier: lowercase alphanumeric or `-`,
/// must start and end with an alphanumeric character, max 253 characters.
pub(crate) fn validate_dns1123_subdomain(value: &str, field: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > 253 {
        return Err(Status::invalid_argument(format!(
            "{field} must be 253 characters or fewer"
        )));
    }
    let is_alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let starts_ok = value.chars().next().is_some_and(is_alnum);
    let ends_ok = value.chars().last().is_some_and(is_alnum);
    let chars_ok = value.chars().all(|c| is_alnum(c) || c == '-');
    if !starts_ok || !ends_ok || !chars_ok {
        return Err(Status::invalid_argument(format!(
            "{field} is invalid: must be lowercase alphanumeric characters or '-', \
             and must start and end with an alphanumeric character"
        )));
    }
    Ok(())
}

/// Validates an opaque identifier: 1-253 characters of ASCII letters, digits, `.`, `-`
/// or `_`.
pub(crate) fn validate_opaque_id(value: &str, field: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > 253 {
        return Err(Status::invalid_argument(format!(
            "{field} must be 253 characters or fewer"
        )));
    }
    let is_allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    if !value.chars().all(is_allowed) {
        return Err(Status::invalid_argument(format!(
            "{field} is invalid: must be ASCII letters, digits, '.', '-' or '_'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn test_opaque_id_accepts_common_generators() {
        for id in [
            "550e8400-e29b-41d4-a716-446655440000",
            "550E8400-E29B-41D4-A716-446655440000",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "node_1.local",
        ] {
            assert!(
                validate_opaque_id(id, "node id").is_ok(),
                "{id} should be accepted"
            );
        }
    }

    #[test]
    fn test_opaque_id_rejects_empty() {
        let err = validate_opaque_id("", "node id").unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "node id must not be empty");
    }

    #[test]
    fn test_opaque_id_rejects_key_breaking_characters() {
        // A '/' would escape its segment in the store's key scheme.
        for id in ["a/b", "a b", "a\tb", "a\nb"] {
            assert_eq!(
                validate_opaque_id(id, "node id").unwrap_err().code(),
                Code::InvalidArgument,
                "{id:?} should be rejected"
            );
        }
    }

    #[test]
    fn test_opaque_id_rejects_overlong() {
        let err = validate_opaque_id(&"a".repeat(254), "node id").unwrap_err();
        assert_eq!(err.message(), "node id must be 253 characters or fewer");
    }
}
