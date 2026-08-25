// Shared test fixtures for building fake domain objects. Only compiled for test builds.

use std::sync::Arc;

use proto::shared::v1::{Node, NodeStatus, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec};

use crate::service::ApiService;
use crate::store::Store;

pub(crate) fn service() -> ApiService {
    ApiService {
        store: Arc::new(Store::new()),
    }
}

pub(crate) fn pod_detail(namespace: &str, name: &str) -> PodDetail {
    PodDetail {
        core: Some(PodWithSpec {
            pod: Some(Pod {
                name: name.to_string(),
                status: PodStatus::Pending as i32,
                requests: None,
                limits: None,
            }),
            spec: Some(PodSpec {
                namespace: namespace.to_string(),
                containers: vec![],
            }),
        }),
        container_statuses: vec![],
        pod_ip: None,
        message: None,
        resource_usage: None,
        node_name: String::new(),
        unschedulable_reason: None,
    }
}

pub(crate) fn node(name: &str, status: NodeStatus) -> Node {
    Node {
        name: name.to_string(),
        status: status as i32,
        capacity: None,
        allocatable: None,
    }
}
