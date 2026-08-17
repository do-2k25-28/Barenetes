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
pub struct Store {
    pods: RwLock<HashMap<(String, String), PodDetail>>,
    nodes: RwLock<HashMap<String, Node>>,
    pod_events: broadcast::Sender<WatchPodEvent>,
    node_events: broadcast::Sender<WatchNodeEvent>,
    // One channel per node, created lazily on first publish/subscribe
    desired_state_channels: RwLock<HashMap<String, broadcast::Sender<WatchDesiredStateEvent>>>,
}

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
