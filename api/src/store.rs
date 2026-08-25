use std::collections::HashMap;
use std::time::Duration;

use proto::api::v1::{WatchDesiredStateEvent, WatchNodeEvent, WatchPodEvent};
use proto::shared::v1::{EventType, Node, NodeStatus, PodDetail};
use tokio::sync::{RwLock, broadcast};
use tokio::time::Instant;

const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Expected interval between UpdateNodeStatus heartbeats from a live agent.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// A node is considered stale after this many missed heartbeats.
pub const HEARTBEAT_TIMEOUT_MULTIPLIER: u32 = 3;
pub const NODE_STALE_TIMEOUT: Duration =
    Duration::from_secs(HEARTBEAT_INTERVAL.as_secs() * HEARTBEAT_TIMEOUT_MULTIPLIER as u64);

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
    node_last_seen: RwLock<HashMap<String, Instant>>,
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
            node_last_seen: RwLock::new(HashMap::new()),
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

    /// Inserts a pod only if no existing pod shares its (namespace, name), atomically under
    /// a single lock acquisition. Returns `false` (leaving the existing pod untouched) if one
    /// already exists, so callers can implement create-once semantics without a separate
    /// check-then-act race between `get_pod` and `upsert_pod`.
    pub async fn create_pod(&self, pod: PodDetail) -> bool {
        let key = pod_key(&pod);
        let mut pods = self.pods.write().await;
        if pods.contains_key(&key) {
            return false;
        }
        let event = WatchPodEvent {
            event_type: EventType::Added as i32,
            pod: Some(pod.clone()),
        };
        pods.insert(key, pod);
        self.publish_pod_event(event);
        true
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

    /// Looks up the pod by (namespace, name) and applies `mutate` to it, then publishes
    /// a MODIFIED event, all under a single write-lock guard so a watcher can never
    /// observe a stale read racing the update. Returns `true` if a pod was found and
    /// updated, `false` if no pod exists for that key.
    pub async fn update_pod_status<F>(&self, namespace: &str, name: &str, mutate: F) -> bool
    where
        F: FnOnce(&mut PodDetail),
    {
        let mut pods = self.pods.write().await;
        let Some(pod) = pods.get_mut(&(namespace.to_string(), name.to_string())) else {
            return false;
        };
        mutate(pod);
        self.publish_pod_event(WatchPodEvent {
            event_type: EventType::Modified as i32,
            pod: Some(pod.clone()),
        });
        true
    }

    /// Inserts or replaces a node and records this call as a liveness heartbeat,
    /// then publishes the resulting ADDED (first report) or MODIFIED event while
    /// still holding the write guard, so a watcher can never observe MODIFIED
    /// before the ADDED of the node it refers to
    pub async fn upsert_and_publish_node(&self, node: Node) {
        let mut last_seen = self.node_last_seen.write().await;
        last_seen.insert(node.name.clone(), Instant::now());

        let mut nodes = self.nodes.write().await;
        let event_type = match nodes.insert(node.name.clone(), node.clone()) {
            None => EventType::Added,
            Some(_) => EventType::Modified,
        };
        self.publish_node_event(WatchNodeEvent {
            event_type: event_type as i32,
            node: Some(node),
        });
    }

    pub async fn get_node(&self, name: &str) -> Option<Node> {
        self.nodes.read().await.get(name).cloned()
    }

    pub async fn list_nodes(&self) -> Vec<Node> {
        self.nodes.read().await.values().cloned().collect()
    }

    /// Marks any node that hasn't reported a heartbeat within `timeout` as NOT_READY,
    /// publishing a node MODIFIED event for each newly-stale node. Returns their names.
    pub async fn sweep_stale_nodes(&self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        let stale_names: Vec<String> = {
            let last_seen = self.node_last_seen.read().await;
            last_seen
                .iter()
                .filter(|(_, seen)| now.saturating_duration_since(**seen) >= timeout)
                .map(|(name, _)| name.clone())
                .collect()
        };

        let mut newly_stale = Vec::new();
        {
            let mut nodes = self.nodes.write().await;
            for name in &stale_names {
                if let Some(node) = nodes.get_mut(name)
                    && node.status != NodeStatus::NotReady as i32
                {
                    node.status = NodeStatus::NotReady as i32;
                    newly_stale.push(node.clone());
                }
            }
        }

        for node in &newly_stale {
            self.publish_node_event(WatchNodeEvent {
                event_type: EventType::Modified as i32,
                node: Some(node.clone()),
            });
        }

        newly_stale.into_iter().map(|node| node.name).collect()
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

pub(crate) fn pod_key(pod: &PodDetail) -> (String, String) {
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
    use proto::shared::v1::NodeStatus;

    use super::*;
    use crate::test_support;

    #[tokio::test]
    async fn test_upsert_and_get_pod() {
        let store = Store::new();
        let pod = test_support::pod_detail("default", "my-pod");

        store.upsert_pod(pod.clone()).await;

        assert_eq!(store.get_pod("default", "my-pod").await, Some(pod));
    }

    #[tokio::test]
    async fn test_get_pod_missing_returns_none() {
        let store = Store::new();

        assert_eq!(store.get_pod("default", "does-not-exist").await, None);
    }

    #[tokio::test]
    async fn test_create_pod_rejects_duplicate_and_leaves_existing_untouched() {
        let store = Store::new();
        let original = test_support::pod_detail("default", "my-pod");
        assert!(store.create_pod(original.clone()).await);

        let mut duplicate = test_support::pod_detail("default", "my-pod");
        duplicate.message = Some("different".to_string());
        assert!(!store.create_pod(duplicate).await);

        assert_eq!(store.get_pod("default", "my-pod").await, Some(original));
    }

    #[tokio::test]
    async fn test_update_pod_status_applies_mutation_and_publishes_modified() {
        let store = Store::new();
        store
            .upsert_pod(test_support::pod_detail("default", "web"))
            .await;
        let mut events = store.subscribe_pod_events();

        let found = store
            .update_pod_status("default", "web", |pod| {
                pod.pod_ip = Some("10.0.0.5".to_string());
            })
            .await;

        assert!(found, "an existing pod should be updated");
        assert_eq!(
            store.get_pod("default", "web").await.unwrap().pod_ip,
            Some("10.0.0.5".to_string())
        );

        let event = events
            .try_recv()
            .expect("a pod update should publish a MODIFIED event");
        assert_eq!(event.event_type, EventType::Modified as i32);
        assert_eq!(event.pod.unwrap().pod_ip, Some("10.0.0.5".to_string()));

        assert!(
            !store.update_pod_status("default", "ghost", |_| {}).await,
            "updating a missing pod should return false"
        );
    }

    #[tokio::test]
    async fn test_upsert_node_replaces_existing() {
        let store = Store::new();
        let mut events = store.subscribe_node_events();

        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await;

        assert_eq!(
            events.try_recv().unwrap().event_type,
            EventType::Added as i32
        );
        assert_eq!(
            events.try_recv().unwrap().event_type,
            EventType::Modified as i32
        );
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

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_marks_unresponsive_node_not_ready() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;
        let mut events = store.subscribe_node_events();

        tokio::time::advance(NODE_STALE_TIMEOUT + Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert_eq!(stale, vec!["node-1".to_string()]);
        let node = store.get_node("node-1").await.unwrap();
        assert_eq!(node.status, NodeStatus::NotReady as i32);
        let event = events
            .try_recv()
            .expect("a node MODIFIED event should have been published");
        assert_eq!(event.event_type, EventType::Modified as i32);
    }

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_marks_node_stale_at_exact_timeout() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;

        tokio::time::advance(NODE_STALE_TIMEOUT).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert_eq!(stale, vec!["node-1".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_leaves_fresh_node_untouched() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;

        tokio::time::advance(Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(stale.is_empty());
        let node = store.get_node("node-1").await.unwrap();
        assert_eq!(node.status, NodeStatus::Ready as i32);
    }

    #[tokio::test(start_paused = true)]
    async fn test_upsert_and_publish_node_refreshes_liveness() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;

        tokio::time::advance(NODE_STALE_TIMEOUT - Duration::from_secs(1)).await;
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await; // heartbeat

        tokio::time::advance(NODE_STALE_TIMEOUT - Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(stale.is_empty(), "recent heartbeat should keep node fresh");
    }

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_does_not_report_already_not_ready_node() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await;

        tokio::time::advance(NODE_STALE_TIMEOUT + Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(
            stale.is_empty(),
            "already-NotReady node shouldn't be reported as newly stale"
        );
    }
}
