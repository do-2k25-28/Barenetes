use std::collections::HashMap;

use proto::api::v1::{WatchDesiredStateEvent, WatchNodeEvent, WatchPodEvent};
use proto::shared::v1::{Node, PodDetail};
use tokio::sync::{RwLock, broadcast};

const EVENT_CHANNEL_CAPACITY: usize = 128;

/// TODO: currently in-memory only for now, will need replacing with a database
// (etcd may be overkill for the minimal version we are trying to achieve,
// so could look at some alternatives)
///
/// Pods are keyed by (namespace, name)
/// Nodes are keyed by name alone (no namespace field on Node).
/// TODO: `allow(dead_code)` is temporary, just for scaffolding purposes to allow
/// handlers to have empty bodies
#[allow(dead_code)]
pub struct Store {
    pods: RwLock<HashMap<(String, String), PodDetail>>,
    nodes: RwLock<HashMap<String, Node>>,
    pod_events: broadcast::Sender<WatchPodEvent>,
    node_events: broadcast::Sender<WatchNodeEvent>,
    // One channel per node, created lazily on first publish/subscribe
    desired_state_channels: RwLock<HashMap<String, broadcast::Sender<WatchDesiredStateEvent>>>,
}

#[allow(dead_code)]
impl Store {
    pub fn new() -> Self {
        Self {
            pods: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            pod_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            desired_state_channels: RwLock::new(HashMap::new()),
        }
    }

    /// Inserts a pod, replacing any existing entry with the same (namespace, name).
    pub async fn upsert_pod(&self, pod: PodDetail) {
        let key = pod_key(&pod);
        self.pods.write().await.insert(key, pod);
    }

    pub async fn get_pod(&self, namespace: &str, name: &str) -> Option<PodDetail> {
        self.pods
            .read()
            .await
            .get(&(namespace.to_string(), name.to_string()))
            .cloned()
    }

    pub async fn list_pods(&self) -> Vec<PodDetail> {
        self.pods.read().await.values().cloned().collect()
    }

    pub async fn remove_pod(&self, namespace: &str, name: &str) -> Option<PodDetail> {
        self.pods
            .write()
            .await
            .remove(&(namespace.to_string(), name.to_string()))
    }

    /// Inserts or replaces a node
    pub async fn upsert_node(&self, node: Node) {
        self.nodes.write().await.insert(node.name.clone(), node);
    }

    pub async fn get_node(&self, name: &str) -> Option<Node> {
        self.nodes.read().await.get(name).cloned()
    }

    pub async fn list_nodes(&self) -> Vec<Node> {
        self.nodes.read().await.values().cloned().collect()
    }

    // Publish is fire-and-forget: a `send` error just means nobody is subscribed yet,
    // which isn't a failure condition for an event that nobody asked to watch.
    pub fn publish_pod_event(&self, event: WatchPodEvent) {
        let _ = self.pod_events.send(event);
    }

    pub fn subscribe_pod_events(&self) -> broadcast::Receiver<WatchPodEvent> {
        self.pod_events.subscribe()
    }

    pub fn publish_node_event(&self, event: WatchNodeEvent) {
        let _ = self.node_events.send(event);
    }

    pub fn subscribe_node_events(&self) -> broadcast::Receiver<WatchNodeEvent> {
        self.node_events.subscribe()
    }

    pub async fn publish_desired_state_event(
        &self,
        node_name: &str,
        event: WatchDesiredStateEvent,
    ) {
        let sender = self.desired_state_sender(node_name).await;
        let _ = sender.send(event);
    }

    pub async fn subscribe_desired_state_events(
        &self,
        node_name: &str,
    ) -> broadcast::Receiver<WatchDesiredStateEvent> {
        self.desired_state_sender(node_name).await.subscribe()
    }

    /// Get-or-create: returns the existing sender for `node_name`, or creates a fresh channel for
    /// it if this is the first publish/subscribe seen for that node.
    async fn desired_state_sender(
        &self,
        node_name: &str,
    ) -> broadcast::Sender<WatchDesiredStateEvent> {
        if let Some(sender) = self.desired_state_channels.read().await.get(node_name) {
            return sender.clone();
        }
        self.desired_state_channels
            .write()
            .await
            .entry(node_name.to_string())
            .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0)
            .clone()
    }
}

fn pod_key(pod: &PodDetail) -> (String, String) {
    let namespace = pod
        .core
        .as_ref()
        .and_then(|core| core.spec.as_ref())
        .map(|spec| spec.namespace.clone())
        .unwrap_or_default();
    let name = pod
        .core
        .as_ref()
        .and_then(|core| core.pod.as_ref())
        .map(|inner| inner.name.clone())
        .unwrap_or_default();
    (namespace, name)
}

#[cfg(test)]
mod tests {
    use proto::api::v1::watch_desired_state_event;
    use proto::shared::v1::{NodeStatus, Pod, PodSpec, PodStatus, PodWithSpec};

    use super::*;

    fn get_pod_detail(namespace: &str, name: &str) -> PodDetail {
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
            pod_ip: String::new(),
            message: String::new(),
            resource_usage: None,
            node_name: String::new(),
        }
    }

    fn get_node(name: &str, status: NodeStatus) -> Node {
        Node {
            name: name.to_string(),
            status: status as i32,
            capacity: None,
            allocatable: None,
        }
    }

    #[tokio::test]
    async fn test_upsert_and_get_pod() {
        let store = Store::new();
        let pod = get_pod_detail("default", "my-pod");

        store.upsert_pod(pod.clone()).await;

        assert_eq!(store.get_pod("default", "my-pod").await, Some(pod));
    }

    #[tokio::test]
    async fn test_get_pod_missing_returns_none() {
        let store = Store::new();

        assert_eq!(store.get_pod("default", "does-not-exist").await, None);
    }

    #[tokio::test]
    async fn test_upsert_node_replaces_existing() {
        let store = Store::new();
        store
            .upsert_node(get_node("node-1", NodeStatus::Ready))
            .await;

        store
            .upsert_node(get_node("node-1", NodeStatus::NotReady))
            .await;

        let nodes = store.list_nodes().await;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].status, NodeStatus::NotReady as i32);
    }

    fn get_desired_state_event(
        action: watch_desired_state_event::Action,
    ) -> WatchDesiredStateEvent {
        WatchDesiredStateEvent {
            action: action as i32,
            pod: None,
        }
    }

    #[tokio::test]
    async fn test_desired_state_events_are_scoped_per_node() {
        let store = Store::new();
        let mut node_a = store.subscribe_desired_state_events("node-a").await;
        let mut node_b = store.subscribe_desired_state_events("node-b").await;

        store
            .publish_desired_state_event(
                "node-a",
                get_desired_state_event(watch_desired_state_event::Action::Run),
            )
            .await;

        let event = node_a
            .try_recv()
            .expect("node-a should have received its own event");
        assert_eq!(event.action, watch_desired_state_event::Action::Run as i32);

        assert!(
            node_b.try_recv().is_err(),
            "node-b should not receive an event published to node-a"
        );
    }
}
