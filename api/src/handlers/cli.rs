/// CLI-facing pod & node reads/writes.
use proto::api::v1::{
    CreatePodRequest, CreatePodResponse, DeletePodRequest, DeletePodResponse, GetNodeRequest,
    GetNodeResponse, GetPodRequest, GetPodResponse, ListNodesRequest, ListNodesResponse,
    ListPodsRequest, ListPodsResponse, WatchPodEvent,
};
use proto::shared::v1::{EventType, PodDetail, PodStatus};
use tonic::{Request, Response, Status};

use crate::service::ApiService;

/// Validates a DNS-1123-label-style identifier: lowercase alphanumeric or `-`,
/// must start and end with an alphanumeric character, max 253 characters.
fn validate_dns1123_label(value: &str, field: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!("{field} must not be empty")));
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
            "{field} '{value}' is invalid: must be lowercase alphanumeric characters or '-', \
             and must start and end with an alphanumeric character"
        )));
    }
    Ok(())
}

impl ApiService {
    pub async fn create_pod_impl(
        &self,
        request: Request<CreatePodRequest>,
    ) -> Result<Response<CreatePodResponse>, Status> {
        let pod_with_spec = request
            .into_inner()
            .pod
            .ok_or_else(|| Status::invalid_argument("pod is required"))?;

        let name = pod_with_spec
            .pod
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.pod is required"))?
            .name
            .clone();
        let spec = pod_with_spec
            .spec
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.spec is required"))?;
        let namespace = spec.namespace.clone();

        validate_dns1123_label(&name, "pod name")?;
        validate_dns1123_label(&namespace, "namespace")?;

        if spec.containers.is_empty() {
            return Err(Status::invalid_argument(
                "pod spec must include at least one container",
            ));
        }
        for container in &spec.containers {
            if container.image.is_empty() {
                return Err(Status::invalid_argument(format!(
                    "container '{}' must specify an image",
                    container.name
                )));
            }
        }

        if self.store.get_pod(&namespace, &name).await.is_some() {
            return Err(Status::already_exists(format!(
                "pod {namespace}/{name} already exists"
            )));
        }

        let mut pod_with_spec = pod_with_spec;
        if let Some(pod) = pod_with_spec.pod.as_mut() {
            pod.status = PodStatus::Pending as i32;
        }

        let pod_detail = PodDetail {
            core: Some(pod_with_spec),
            container_statuses: vec![],
            pod_ip: String::new(),
            message: String::new(),
            resource_usage: None,
            node_name: String::new(),
            unschedulable_reason: None,
        };

        self.store.upsert_pod(pod_detail.clone()).await;
        self.store.publish_pod_event(WatchPodEvent {
            event_type: EventType::Added as i32,
            pod: Some(pod_detail.clone()),
        });

        Ok(Response::new(CreatePodResponse {
            pod: Some(pod_detail),
        }))
    }

    pub async fn delete_pod_impl(
        &self,
        _request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        todo!("store.remove_pod, return NotFound if it didn't exist")
    }

    pub async fn get_pod_impl(
        &self,
        _request: Request<GetPodRequest>,
    ) -> Result<Response<GetPodResponse>, Status> {
        todo!("store.get_pod, return NotFound if missing")
    }

    pub async fn list_pods_impl(
        &self,
        _request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        todo!("store.list_pods")
    }

    pub async fn get_node_impl(
        &self,
        _request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        todo!("store.get_node, return NotFound if missing")
    }

    pub async fn list_nodes_impl(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        todo!("store.list_nodes")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proto::shared::v1::{Container, Pod, PodSpec, PodWithSpec};
    use tonic::Code;

    use super::*;
    use crate::store::Store;

    fn service() -> ApiService {
        ApiService {
            store: Arc::new(Store::new()),
        }
    }

    fn valid_container() -> Container {
        Container {
            name: "app".to_string(),
            image: "busybox".to_string(),
            ports: vec![],
            env: vec![],
        }
    }

    fn create_pod_request(namespace: &str, name: &str) -> Request<CreatePodRequest> {
        Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    status: PodStatus::Running as i32, // caller-supplied status must be ignored
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![valid_container()],
                }),
            }),
        })
    }

    #[tokio::test]
    async fn test_create_pod_returns_pod_detail_with_pending_status() {
        let service = service();

        let response = service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed")
            .into_inner();

        let core = response.pod.expect("response should contain a pod").core;
        let pod = core.as_ref().and_then(|c| c.pod.as_ref()).unwrap();
        let spec = core.as_ref().and_then(|c| c.spec.as_ref()).unwrap();
        assert_eq!(pod.name, "my-pod");
        assert_eq!(pod.status, PodStatus::Pending as i32);
        assert_eq!(spec.namespace, "default");
    }

    #[tokio::test]
    async fn test_create_pod_persists_to_store() {
        let service = service();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .expect("pod should be in the store");
        assert_eq!(stored.node_name, "");
    }

    #[tokio::test]
    async fn test_create_pod_duplicate_returns_already_exists() {
        let service = service();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("first create_pod should succeed");

        let err = service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect_err("second create_pod should fail");

        assert_eq!(err.code(), Code::AlreadyExists);
        assert_eq!(service.store.list_pods().await.len(), 1);
    }

    #[tokio::test]
    async fn test_create_pod_publishes_added_event() {
        let service = service();
        let mut events = service.store.subscribe_pod_events();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let event = events
            .try_recv()
            .expect("an event should have been published");
        assert_eq!(event.event_type, EventType::Added as i32);
        let pod_name = event
            .pod
            .and_then(|p| p.core)
            .and_then(|c| c.pod)
            .map(|p| p.name);
        assert_eq!(pod_name.as_deref(), Some("my-pod"));
    }

    #[tokio::test]
    async fn test_create_pod_missing_pod_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(Request::new(CreatePodRequest { pod: None }))
            .await
            .expect_err("missing pod should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_missing_spec_is_invalid_argument() {
        let service = service();
        let request = Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: "my-pod".to_string(),
                    status: PodStatus::Pending as i32,
                    requests: None,
                    limits: None,
                }),
                spec: None,
            }),
        });

        let err = service
            .create_pod_impl(request)
            .await
            .expect_err("missing spec should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_empty_name_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("default", ""))
            .await
            .expect_err("empty name should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_invalid_name_charset_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("default", "My_Pod"))
            .await
            .expect_err("invalid name charset should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_empty_namespace_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("", "my-pod"))
            .await
            .expect_err("empty namespace should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_invalid_namespace_charset_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("Default_NS", "my-pod"))
            .await
            .expect_err("invalid namespace charset should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_no_containers_is_invalid_argument() {
        let service = service();
        let request = Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: "my-pod".to_string(),
                    status: PodStatus::Pending as i32,
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: "default".to_string(),
                    containers: vec![],
                }),
            }),
        });

        let err = service
            .create_pod_impl(request)
            .await
            .expect_err("no containers should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_container_missing_image_is_invalid_argument() {
        let service = service();
        let mut container = valid_container();
        container.image = String::new();
        let request = Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: "my-pod".to_string(),
                    status: PodStatus::Pending as i32,
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: "default".to_string(),
                    containers: vec![container],
                }),
            }),
        });

        let err = service
            .create_pod_impl(request)
            .await
            .expect_err("missing image should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }
}
