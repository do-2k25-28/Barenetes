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
