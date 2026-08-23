// Shared gRPC error-status constructors, so every handler reports "not found"/"already
// exists" the same way instead of hand-building a message per call site.
use tonic::Status;

pub(crate) fn pod_not_found(namespace: &str, name: &str) -> Status {
    Status::not_found(format!("pod {namespace}/{name} not found"))
}

pub(crate) fn node_not_found(name: &str) -> Status {
    Status::not_found(format!("node {name} not found"))
}

pub(crate) fn missing_node(name: &str) -> Status {
    Status::invalid_argument(format!("missing node {name}"))
}

#[allow(dead_code)]
pub(crate) fn pod_already_exists(namespace: &str, name: &str) -> Status {
    Status::already_exists(format!("pod {namespace}/{name} already exists"))
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn test_pod_not_found() {
        let status = pod_not_found("default", "my-pod");
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "pod default/my-pod not found");
    }

    #[test]
    fn test_node_not_found() {
        let status = node_not_found("node-1");
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "node node-1 not found");
    }

    #[test]
    fn test_pod_already_exists() {
        let status = pod_already_exists("default", "my-pod");
        assert_eq!(status.code(), Code::AlreadyExists);
        assert_eq!(status.message(), "pod default/my-pod already exists");
    }

    #[test]
    fn test_missing_node() {
        let status = missing_node("node-1");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "missing node node-1");
    }
}
