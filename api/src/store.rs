use std::collections::HashMap;

use proto::api::v1::{WatchDesiredStateEvent, WatchNodeEvent, WatchPodEvent};
use proto::shared::v1::{EventType, Node, NodeStatus, PodDetail, PodWithSpec};
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

    /// Removes the pod by (namespace, name) and publishes a DELETED event under the same
    /// write-lock guard, so a watcher can never observe the removal without the event, nor
    /// see it out of order against a recreation of the same key. Returns the removed pod,
    /// or `None` (publishing nothing) if no pod exists for that key.
    pub async fn remove_pod(&self, namespace: &str, name: &str) -> Option<PodDetail> {
        let mut pods = self.pods.write().await;
        let pod = pods.remove(&(namespace.to_string(), name.to_string()))?;
        self.publish_pod_event(WatchPodEvent {
            event_type: EventType::Deleted as i32,
            pod: Some(pod.clone()),
        });
        Some(pod)
    }

    /// Looks up the pod by (namespace, name) and applies `mutate` to it, then publishes
    /// `event_type`, all under a single write-lock guard so a watcher can never
    /// observe a stale read racing the update. Returns `true` if a pod was found and
    /// updated, `false` if no pod exists for that key.
    pub async fn update_and_publish_pod<F>(
        &self,
        namespace: &str,
        name: &str,
        event_type: EventType,
        mutate: F,
    ) -> bool
    where
        F: FnOnce(&mut PodDetail),
    {
        let mut pods = self.pods.write().await;
        let Some(pod) = pods.get_mut(&(namespace.to_string(), name.to_string())) else {
            return false;
        };
        mutate(pod);
        self.publish_pod_event(WatchPodEvent {
            event_type: event_type as i32,
            pod: Some(pod.clone()),
        });
        true
    }

    pub async fn update_pod_status<F>(&self, namespace: &str, name: &str, mutate: F) -> bool
    where
        F: FnOnce(&mut PodDetail),
    {
        self.update_and_publish_pod(namespace, name, EventType::Modified, mutate)
            .await
    }

    /// Inserts or replaces a node and records this call as a liveness heartbeat,
    /// then publishes the resulting ADDED (first report) or MODIFIED event while
    /// still holding the write guard, so a watcher can never observe MODIFIED
    /// before the ADDED of the node it refers to
    pub async fn upsert_and_publish_node(&self, node: Node) {
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

    /// Sets the node's status and publishes the resulting MODIFIED event under the same
    /// write guard, so a watcher can't observe the change without the event. Returns
    /// `false` if no node exists by that name, or if it already had that status.
    pub async fn set_node_status(&self, name: &str, status: NodeStatus) -> bool {
        let mut nodes = self.nodes.write().await;
        let Some(node) = nodes.get_mut(name) else {
            return false;
        };
        if node.status == status as i32 {
            return false;
        }
        node.status = status as i32;
        self.publish_node_event(WatchNodeEvent {
            event_type: EventType::Modified as i32,
            node: Some(node.clone()),
        });
        true
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
        if let Some(sender) = self.desired_state_channels.read().await.get(node_name) {
            let _ = sender.send(event);
        }
    }

    /// Snapshots the pods currently assigned to `node_name` and subscribes to its
    /// desired-state channel under a single `pods` read guard, so no assignment or
    /// deletion can slip between the two and be missed by the caller.
    pub async fn subscribe_desired_state_with_snapshot(
        &self,
        node_name: &str,
    ) -> (
        Vec<PodWithSpec>,
        broadcast::Receiver<WatchDesiredStateEvent>,
    ) {
        let pods = self.pods.read().await;
        let assigned = pods
            .values()
            .filter(|pod| pod.node_name == node_name)
            .filter_map(|pod| pod.core.clone())
            .collect();
        let receiver = self.subscribe_desired_state_events(node_name).await;

        (assigned, receiver)
    }

    /// Get-or-create the channel for `node_name` and subscribe to it, evicting channels
    /// nobody is listening on any more along the way
    pub async fn subscribe_desired_state_events(
        &self,
        node_name: &str,
    ) -> broadcast::Receiver<WatchDesiredStateEvent> {
        let mut channels = self.desired_state_channels.write().await;

        channels.retain(|_, sender| sender.receiver_count() > 0);

        channels
            .entry(node_name.to_string())
            .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    #[cfg(test)]
    pub(crate) async fn desired_state_channel_count(&self) -> usize {
        self.desired_state_channels.read().await.len()
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
    async fn test_remove_pod_publishes_deleted() {
        let store = Store::new();
        store
            .upsert_pod(test_support::pod_detail("default", "my-pod"))
            .await;
        let mut events = store.subscribe_pod_events();

        let removed = store.remove_pod("default", "my-pod").await;

        assert!(removed.is_some());
        let event = events
            .try_recv()
            .expect("removing a pod should publish an event");
        assert_eq!(event.event_type, EventType::Deleted as i32);
        assert_eq!(event.pod, removed);
    }

    #[tokio::test]
    async fn test_remove_pod_missing_publishes_nothing() {
        let store = Store::new();
        let mut events = store.subscribe_pod_events();

        assert!(store.remove_pod("default", "ghost").await.is_none());
        assert!(
            events.try_recv().is_err(),
            "removing a pod that doesn't exist must publish nothing"
        );
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
    async fn test_update_and_publish_pod_uses_the_given_event_type() {
        let store = Store::new();
        store
            .upsert_pod(test_support::pod_detail("default", "my-pod"))
            .await;
        let mut events = store.subscribe_pod_events();

        let found = store
            .update_and_publish_pod("default", "my-pod", EventType::Scheduled, |pod| {
                pod.node_name = "node-1".to_string();
            })
            .await;

        assert!(found);
        let event = events.try_recv().expect("an event should be published");
        assert_eq!(event.event_type, EventType::Scheduled as i32);
        assert_eq!(event.pod.unwrap().node_name, "node-1");
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
    async fn test_set_node_status_publishes_modified() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;
        let mut events = store.subscribe_node_events();

        assert!(store.set_node_status("node-1", NodeStatus::NotReady).await);

        let event = events.try_recv().expect("a node event should be published");
        assert_eq!(event.event_type, EventType::Modified as i32);
        assert_eq!(event.node.unwrap().status, NodeStatus::NotReady as i32);
    }

    #[tokio::test]
    async fn test_set_node_status_is_a_noop_when_unchanged() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;
        let mut events = store.subscribe_node_events();

        assert!(!store.set_node_status("node-1", NodeStatus::Ready).await);
        assert!(
            events.try_recv().is_err(),
            "an unchanged status must not publish an event"
        );
    }

    #[tokio::test]
    async fn test_set_node_status_unknown_node_is_a_noop() {
        let store = Store::new();
        assert!(!store.set_node_status("ghost", NodeStatus::NotReady).await);
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
    async fn test_publish_desired_state_to_unwatched_node_creates_no_channel() {
        let store = Store::new();

        store
            .publish_desired_state_event(
                "node-a",
                get_desired_state_event(watch_desired_state_event::Action::Run),
            )
            .await;

        assert_eq!(
            store.desired_state_channel_count().await,
            0,
            "publishing to a node nobody watches must not leave a channel behind"
        );
    }

    #[tokio::test]
    async fn test_desired_state_channel_dropped_once_last_watcher_goes_away() {
        let store = Store::new();

        let receiver = store.subscribe_desired_state_events("node-a").await;
        assert_eq!(store.desired_state_channel_count().await, 1);

        drop(receiver);

        // The eviction happens on the next subscribe, so node-b's arrival should clear node-a.
        let _node_b = store.subscribe_desired_state_events("node-b").await;
        assert_eq!(
            store.desired_state_channel_count().await,
            1,
            "node-a's channel should have been evicted once its only receiver was dropped"
        );
    }

    #[tokio::test]
    async fn test_desired_state_channel_kept_while_another_watcher_remains() {
        let store = Store::new();

        let first = store.subscribe_desired_state_events("node-a").await;
        let mut second = store.subscribe_desired_state_events("node-a").await;
        drop(first);

        // A subscribe for a different node triggers the prune so node-a still has a receiver.
        let _node_b = store.subscribe_desired_state_events("node-b").await;

        store
            .publish_desired_state_event(
                "node-a",
                get_desired_state_event(watch_desired_state_event::Action::Run),
            )
            .await;

        assert!(
            second.try_recv().is_ok(),
            "the surviving node-a watcher must still receive events"
        );
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
