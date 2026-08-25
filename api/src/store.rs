use prost::Message;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use proto::api::v1::{WatchDesiredStateEvent, WatchNodeEvent, WatchPodEvent};
use proto::shared::v1::{EventType, Node, NodeStatus, PodDetail, PodWithSpec};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio::time::Instant;

#[derive(Debug)]
pub enum StoreError {
    Connection(String),
    Decode(String),
}

impl StoreError {
    pub fn to_status(&self) -> tonic::Status {
        match self {
            StoreError::Connection(msg) => tonic::Status::unavailable(msg),
            StoreError::Decode(msg) => tonic::Status::internal(msg),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Connection(msg) => write!(f, "etcd connection error: {msg}"),
            StoreError::Decode(msg) => write!(f, "etcd decode error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<etcd_client::Error> for StoreError {
    fn from(e: etcd_client::Error) -> Self {
        StoreError::Connection(e.to_string())
    }
}

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
    client: Option<etcd_client::Client>,
    pods: RwLock<HashMap<(String, String), PodDetail>>,
    nodes: RwLock<HashMap<String, Node>>,
    node_last_seen: RwLock<HashMap<String, Instant>>,
    pod_events: broadcast::Sender<WatchPodEvent>,
    node_events: broadcast::Sender<WatchNodeEvent>,
    /// Serializes the read-modify-write on a node between heartbeat and sweeper.
    node_op_lock: Mutex<()>,
    // One channel per node, created lazily on first publish/subscribe
    desired_state_channels: RwLock<HashMap<String, broadcast::Sender<WatchDesiredStateEvent>>>,
}

#[allow(dead_code)]
impl Store {
    // In-memory store (used for tests)
    pub fn new() -> Self {
        Self {
            client: None,
            pods: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            node_last_seen: RwLock::new(HashMap::new()),
            pod_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_op_lock: Mutex::new(()),
            desired_state_channels: RwLock::new(HashMap::new()),
        }
    }

    pub fn new_with_etcd(client: etcd_client::Client) -> Self {
        Self {
            client: Some(client),
            pods: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            node_last_seen: RwLock::new(HashMap::new()),
            pod_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_op_lock: Mutex::new(()),
            desired_state_channels: RwLock::new(HashMap::new()),
        }
    }

    /// Populate `node_last_seen` from persisted etcd nodes so the sweeper
    /// can see them. Each node is marked as "just seen" so it must send a
    /// heartbeat within the normal timeout to stay Ready.
    pub async fn load_node_liveness(&self) -> Result<(), StoreError> {
        let client = match self.client {
            Some(ref c) => c.clone(),
            None => return Ok(()),
        };
        let resp = client
            .clone()
            .get(
                nodes_prefix(),
                Some(etcd_client::GetOptions::default().with_prefix()),
            )
            .await?;
        let mut last_seen = self.node_last_seen.write().await;
        for kv in resp.kvs() {
            match Node::decode(kv.value()) {
                Ok(node) => {
                    last_seen.insert(node.name, Instant::now());
                }
                Err(e) => {
                    let key = String::from_utf8_lossy(kv.key());
                    tracing::warn!(key = %key, error = %e, "skipping undecodable node at boot");
                    last_seen.insert(key.into_owned(), Instant::now());
                }
            }
        }
        Ok(())
    }

    /// Inserts a pod, replacing any existing entry with the same (namespace, name).
    pub async fn upsert_pod(&self, pod: PodDetail) -> Result<(), StoreError> {
        if let Some(ref client) = self.client {
            let (namespace, name) = pod_key(&pod);
            let etcd_key = pod_etcd_key(&namespace, &name);
            let value = pod.encode_to_vec();
            client.clone().put(etcd_key, value, None).await?;
        } else {
            let key = pod_key(&pod);
            self.pods.write().await.insert(key, pod);
        }
        Ok(())
    }

    /// Inserts a pod only if no existing pod shares its (namespace, name).
    /// Returns `false` (leaving the existing pod untouched) if one already exists.
    pub async fn create_pod(&self, pod: PodDetail) -> Result<bool, StoreError> {
        let (namespace, name) = pod_key(&pod);
        if let Some(ref client) = self.client {
            let etcd_key = pod_etcd_key(&namespace, &name);
            let txn = etcd_client::Txn::new()
                .when(vec![etcd_client::Compare::version(
                    etcd_key.clone(),
                    etcd_client::CompareOp::Equal,
                    0,
                )])
                .and_then(vec![etcd_client::TxnOp::put(
                    etcd_key,
                    pod.encode_to_vec(),
                    None,
                )]);
            let resp = client.clone().txn(txn).await?;
            if resp.succeeded() {
                self.publish_pod_event(WatchPodEvent {
                    event_type: EventType::Added as i32,
                    pod: Some(pod),
                });
            }
            return Ok(resp.succeeded());
        } else {
            let key = (namespace, name);
            let mut pods = self.pods.write().await;
            if pods.contains_key(&key) {
                return Ok(false);
            }
            pods.insert(key, pod.clone());
        }
        self.publish_pod_event(WatchPodEvent {
            event_type: EventType::Added as i32,
            pod: Some(pod),
        });
        Ok(true)
    }

    pub async fn get_pod(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<PodDetail>, StoreError> {
        if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(pod_etcd_key(namespace, name), None)
                .await?;
            let kv = match resp.kvs().first() {
                Some(kv) => kv,
                None => return Ok(None),
            };
            Ok(PodDetail::decode(kv.value())
                .map(Some)
                .map_err(|e| StoreError::Decode(e.to_string()))?)
        } else {
            Ok(self
                .pods
                .read()
                .await
                .get(&(namespace.to_string(), name.to_string()))
                .cloned())
        }
    }

    pub async fn list_pods(&self) -> Result<Vec<PodDetail>, StoreError> {
        if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(
                    pods_prefix(),
                    Some(etcd_client::GetOptions::default().with_prefix()),
                )
                .await?;
            Ok(resp
                .kvs()
                .iter()
                .filter_map(|kv| {
                    PodDetail::decode(kv.value())
                        .map_err(|e| {
                            let key = String::from_utf8_lossy(kv.key());
                            tracing::warn!(key = %key, error = %e, "skipping undecodable pod");
                        })
                        .ok()
                })
                .collect())
        } else {
            Ok(self.pods.read().await.values().cloned().collect())
        }
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

    /// Looks up the pod by (namespace, name) and applies `mutate` to it, then
    /// publishes `event_type`. Returns `true` if a pod was found and updated,
    /// `false` if no pod exists for that key.
    pub async fn update_and_publish_pod<F>(
        &self,
        namespace: &str,
        name: &str,
        event_type: EventType,
        mutate: F,
    ) -> Result<bool, StoreError>
    where
        F: FnOnce(&mut PodDetail),
    {
        if let Some(ref client) = self.client {
            let etcd_key = pod_etcd_key(namespace, name);
            let resp = client.clone().get(etcd_key.clone(), None).await?;
            let kv = match resp.kvs().first() {
                Some(kv) => kv,
                None => return Ok(false),
            };
            let version = kv.version();
            let mut pod =
                PodDetail::decode(kv.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
            mutate(&mut pod);
            let txn = etcd_client::Txn::new()
                .when(vec![etcd_client::Compare::version(
                    etcd_key.clone(),
                    etcd_client::CompareOp::Equal,
                    version,
                )])
                .and_then(vec![etcd_client::TxnOp::put(
                    etcd_key,
                    pod.encode_to_vec(),
                    None,
                )]);
            let txn_resp = client.clone().txn(txn).await?;
            if !txn_resp.succeeded() {
                return Ok(false);
            }
            self.publish_pod_event(WatchPodEvent {
                event_type: event_type as i32,
                pod: Some(pod),
            });
        } else {
            let mut pods = self.pods.write().await;
            let Some(pod) = pods.get_mut(&(namespace.to_string(), name.to_string())) else {
                return Ok(false);
            };
            mutate(pod);
            self.publish_pod_event(WatchPodEvent {
                event_type: event_type as i32,
                pod: Some(pod.clone()),
            });
        }
        Ok(true)
    }

    /// Looks up the pod by (namespace, name) and applies `mutate` to it, then
    /// persists to etcd and publishes a MODIFIED event. Uses an etcd Txn to
    /// ensure the key version hasn't changed between read and write. Returns
    /// `true` if updated, `false` if the pod was not found or the CAS failed.
    pub async fn update_pod_status<F>(
        &self,
        namespace: &str,
        name: &str,
        mutate: F,
    ) -> Result<bool, StoreError>
    where
        F: FnOnce(&mut PodDetail),
    {
        if let Some(ref client) = self.client {
            let etcd_key = pod_etcd_key(namespace, name);
            let resp = client.clone().get(etcd_key.clone(), None).await?;
            let kv = match resp.kvs().first() {
                Some(kv) => kv,
                None => return Ok(false),
            };
            let version = kv.version();
            let mut pod =
                PodDetail::decode(kv.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
            mutate(&mut pod);
            let txn = etcd_client::Txn::new()
                .when(vec![etcd_client::Compare::version(
                    etcd_key.clone(),
                    etcd_client::CompareOp::Equal,
                    version,
                )])
                .and_then(vec![etcd_client::TxnOp::put(
                    etcd_key,
                    pod.encode_to_vec(),
                    None,
                )]);
            let txn_resp = client.clone().txn(txn).await?;
            if !txn_resp.succeeded() {
                return Ok(false);
            }
            self.publish_pod_event(WatchPodEvent {
                event_type: EventType::Modified as i32,
                pod: Some(pod),
            });
        } else {
            let mut pods = self.pods.write().await;
            let Some(pod) = pods.get_mut(&(namespace.to_string(), name.to_string())) else {
                return Ok(false);
            };
            mutate(pod);
            self.publish_pod_event(WatchPodEvent {
                event_type: EventType::Modified as i32,
                pod: Some(pod.clone()),
            });
        }
        Ok(true)
    }

    /// Inserts or replaces a node and records this call as a liveness heartbeat,
    /// then publishes the resulting ADDED (first report) or MODIFIED event.
    pub async fn upsert_and_publish_node(&self, node: Node) -> Result<(), StoreError> {
        // Short lock: just update the heartbeat timestamp.
        {
            let mut last_seen = self.node_last_seen.write().await;
            last_seen.insert(node.name.clone(), Instant::now());
        }

        let event_type = if let Some(ref client) = self.client {
            // Serialize the get→put against the sweeper so a heartbeat can't
            // race with a stale put overwriting fresh data.
            let _guard = self.node_op_lock.lock().await;
            let key = node_etcd_key(&node.name);
            let resp = client.clone().get(key.clone(), None).await?;
            let is_new = resp.kvs().is_empty();
            client.clone().put(key, node.encode_to_vec(), None).await?;
            if is_new {
                EventType::Added
            } else {
                EventType::Modified
            }
        } else {
            let mut nodes = self.nodes.write().await;
            match nodes.insert(node.name.clone(), node.clone()) {
                None => EventType::Added,
                Some(_) => EventType::Modified,
            }
        };

        self.publish_node_event(WatchNodeEvent {
            event_type: event_type as i32,
            node: Some(node),
        });
        Ok(())
    }

    pub async fn get_node(&self, name: &str) -> Result<Option<Node>, StoreError> {
        if let Some(ref client) = self.client {
            let resp = client.clone().get(node_etcd_key(name), None).await?;
            let kv = match resp.kvs().first() {
                Some(kv) => kv,
                None => return Ok(None),
            };
            Ok(Node::decode(kv.value())
                .map(Some)
                .map_err(|e| StoreError::Decode(e.to_string()))?)
        } else {
            Ok(self.nodes.read().await.get(name).cloned())
        }
    }

    pub async fn list_nodes(&self) -> Result<Vec<Node>, StoreError> {
        if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(
                    nodes_prefix(),
                    Some(etcd_client::GetOptions::default().with_prefix()),
                )
                .await?;
            Ok(resp
                .kvs()
                .iter()
                .filter_map(|kv| {
                    Node::decode(kv.value())
                        .map_err(|e| {
                            let key = String::from_utf8_lossy(kv.key());
                            tracing::warn!(key = %key, error = %e, "skipping undecodable node");
                        })
                        .ok()
                })
                .collect())
        } else {
            Ok(self.nodes.read().await.values().cloned().collect())
        }
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

        if let Some(ref client) = self.client {
            let _guard = self.node_op_lock.lock().await;
            for name in &stale_names {
                let key = node_etcd_key(name);
                let resp = match client.clone().get(key.clone(), None).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        tracing::warn!(%name, %e, "sweep: failed to read node from etcd");
                        continue;
                    }
                };
                let Some(kv) = resp.kvs().first() else {
                    continue;
                };
                let Ok(mut node) = Node::decode(kv.value()) else {
                    continue;
                };
                if node.status == NodeStatus::NotReady as i32 {
                    continue;
                }
                node.status = NodeStatus::NotReady as i32;
                if let Err(e) = client.clone().put(key, node.encode_to_vec(), None).await {
                    tracing::warn!(%name, %e, "sweep: failed to write node to etcd");
                    continue;
                }
                newly_stale.push(node);
            }
        } else {
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

fn pod_etcd_key(namespace: &str, name: &str) -> Vec<u8> {
    format!("/barenetes/pods/{namespace}/{name}").into_bytes()
}

fn node_etcd_key(name: &str) -> Vec<u8> {
    format!("/barenetes/nodes/{name}").into_bytes()
}

fn pods_prefix() -> Vec<u8> {
    b"/barenetes/pods/".to_vec()
}

fn nodes_prefix() -> Vec<u8> {
    b"/barenetes/nodes/".to_vec()
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

        store.upsert_pod(pod.clone()).await.unwrap();

        assert_eq!(store.get_pod("default", "my-pod").await.unwrap(), Some(pod));
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

        assert_eq!(
            store.get_pod("default", "does-not-exist").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn test_create_pod_rejects_duplicate_and_leaves_existing_untouched() {
        let store = Store::new();
        let original = test_support::pod_detail("default", "my-pod");
        assert!(store.create_pod(original.clone()).await.unwrap());

        let mut duplicate = test_support::pod_detail("default", "my-pod");
        duplicate.message = Some("different".to_string());
        assert!(!store.create_pod(duplicate).await.unwrap());

        assert_eq!(
            store.get_pod("default", "my-pod").await.unwrap(),
            Some(original)
        );
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
            .await
            .unwrap();

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
            .await
            .unwrap();
        let mut events = store.subscribe_pod_events();

        let found = store
            .update_pod_status("default", "web", |pod| {
                pod.pod_ip = Some("10.0.0.5".to_string());
            })
            .await
            .unwrap();

        assert!(found, "an existing pod should be updated");
        assert_eq!(
            store
                .get_pod("default", "web")
                .await
                .unwrap()
                .unwrap()
                .pod_ip,
            Some("10.0.0.5".to_string())
        );

        let event = events
            .try_recv()
            .expect("a pod update should publish a MODIFIED event");
        assert_eq!(event.event_type, EventType::Modified as i32);
        assert_eq!(event.pod.unwrap().pod_ip, Some("10.0.0.5".to_string()));

        assert!(
            !store
                .update_pod_status("default", "ghost", |_| {})
                .await
                .unwrap(),
            "updating a missing pod should return false"
        );
    }

    #[tokio::test]
    async fn test_upsert_node_replaces_existing() {
        let store = Store::new();
        let mut events = store.subscribe_node_events();

        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await
            .unwrap();

        assert_eq!(
            events.try_recv().unwrap().event_type,
            EventType::Added as i32
        );
        assert_eq!(
            events.try_recv().unwrap().event_type,
            EventType::Modified as i32
        );
        let nodes = store.list_nodes().await.unwrap();
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

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_marks_unresponsive_node_not_ready() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();
        let mut events = store.subscribe_node_events();

        tokio::time::advance(NODE_STALE_TIMEOUT + Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert_eq!(stale, vec!["node-1".to_string()]);
        let node = store.get_node("node-1").await.unwrap().unwrap();
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
            .await
            .unwrap();

        tokio::time::advance(NODE_STALE_TIMEOUT).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert_eq!(stale, vec!["node-1".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_leaves_fresh_node_untouched() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        tokio::time::advance(Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(stale.is_empty());
        let node = store.get_node("node-1").await.unwrap().unwrap();
        assert_eq!(node.status, NodeStatus::Ready as i32);
    }

    #[tokio::test(start_paused = true)]
    async fn test_upsert_and_publish_node_refreshes_liveness() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        tokio::time::advance(NODE_STALE_TIMEOUT - Duration::from_secs(1)).await;
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap(); // heartbeat

        tokio::time::advance(NODE_STALE_TIMEOUT - Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(stale.is_empty(), "recent heartbeat should keep node fresh");
    }

    #[tokio::test(start_paused = true)]
    async fn test_sweep_stale_nodes_does_not_report_already_not_ready_node() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await
            .unwrap();

        tokio::time::advance(NODE_STALE_TIMEOUT + Duration::from_secs(1)).await;
        let stale = store.sweep_stale_nodes(NODE_STALE_TIMEOUT).await;

        assert!(
            stale.is_empty(),
            "already-NotReady node shouldn't be reported as newly stale"
        );
    }
}
