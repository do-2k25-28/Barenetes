use prost::Message;
use std::collections::HashMap;
use std::fmt;

use proto::api::v1::{WatchDesiredStateEvent, WatchNodeEvent, WatchPodEvent};
use proto::shared::v1::{EventType, Node, NodeStatus, PodDetail, PodWithSpec};
use tokio::sync::{Mutex, RwLock, broadcast};

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
    pod_events: broadcast::Sender<WatchPodEvent>,
    node_events: broadcast::Sender<WatchNodeEvent>,
    /// Serializes the read-modify-write on a node between registration and the
    /// connect/disconnect transitions driven by the desired-state stream.
    node_op_lock: Mutex<()>,
    /// How many desired-state streams are open per node. A node is READY while this is
    /// above zero. Counting rather than flipping a flag is what makes a reconnect safe:
    /// the outgoing stream's teardown is queued on the runtime and can land after the
    /// replacement has already opened.
    node_watchers: Mutex<HashMap<String, usize>>,
    // One channel per node, created lazily on first publish/subscribe
    desired_state_channels: RwLock<HashMap<String, broadcast::Sender<WatchDesiredStateEvent>>>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl Store {
    // In-memory store (used for tests)
    pub fn new() -> Self {
        Self {
            client: None,
            pods: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            pod_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_op_lock: Mutex::new(()),
            node_watchers: Mutex::new(HashMap::new()),
            desired_state_channels: RwLock::new(HashMap::new()),
        }
    }

    pub fn new_with_etcd(client: etcd_client::Client) -> Self {
        Self {
            client: Some(client),
            pods: RwLock::new(HashMap::new()),
            nodes: RwLock::new(HashMap::new()),
            pod_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            node_op_lock: Mutex::new(()),
            node_watchers: Mutex::new(HashMap::new()),
            desired_state_channels: RwLock::new(HashMap::new()),
        }
    }

    /// Marks every persisted node NOT_READY at boot.
    ///
    /// Node status survives a restart in etcd, but the streams that prove a node is alive
    /// do not. Without this the server would come back advertising nodes as READY on the
    /// strength of a connection that died with the previous process. Each node returns to
    /// READY when its agent opens a desired-state stream.
    pub async fn reset_node_liveness(&self) -> Result<(), StoreError> {
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
        let _guard = self.node_op_lock.lock().await;
        for kv in resp.kvs() {
            let mut node = match Node::decode(kv.value()) {
                Ok(node) => node,
                Err(e) => {
                    let key = String::from_utf8_lossy(kv.key());
                    tracing::warn!(key = %key, error = %e, "skipping undecodable node at boot");
                    continue;
                }
            };
            // Only liveness is reset. CORDON and DRAIN are an operator's decision and
            // have nothing to do with whether the node's agent is connected.
            if node.status != NodeStatus::Ready as i32 {
                continue;
            }
            node.status = NodeStatus::NotReady as i32;
            // Boot-critical: carrying on would serve a node as READY with no agent
            // behind it, and there is no sweeper left to notice.
            client
                .clone()
                .put(node_etcd_key(&node.name), node.encode_to_vec(), None)
                .await?;
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

    /// Removes the pod by (namespace, name) and publishes a DELETED event.
    /// Returns the removed pod, or `None` if no pod exists for that key.
    pub async fn remove_pod(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<PodDetail>, StoreError> {
        if let Some(ref client) = self.client {
            let key = pod_etcd_key(namespace, name);
            let resp = client.clone().get(key.clone(), None).await?;
            let old = resp.kvs().first().and_then(|kv| {
                PodDetail::decode(kv.value())
                    .map_err(|e| {
                        let etcd_key = String::from_utf8_lossy(kv.key());
                        tracing::warn!(key = %etcd_key, error = %e, "skipping undecodable pod for delete");
                    })
                    .ok()
            });
            let _ = client.clone().delete(key, None).await?;
            if let Some(ref pod) = old {
                self.publish_pod_event(WatchPodEvent {
                    event_type: EventType::Deleted as i32,
                    pod: Some(pod.clone()),
                });
            }
            Ok(old)
        } else {
            let mut pods = self.pods.write().await;
            let pod = pods.remove(&(namespace.to_string(), name.to_string()));
            if let Some(ref p) = pod {
                self.publish_pod_event(WatchPodEvent {
                    event_type: EventType::Deleted as i32,
                    pod: Some(p.clone()),
                });
            }
            Ok(pod)
        }
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

    /// Registers an agent's report of a node, preserving whatever liveness status the
    /// store already holds: the agent owns capacity and allocatable, the API owns status.
    /// A node the store has never seen starts NOT_READY (it has no stream yet), and READY
    /// is the proto3 default, so trusting the reported field would mark it alive with no
    /// stream. Publishes ADDED on first registration, MODIFIED afterwards.
    ///
    /// The read and the write share one lock acquisition, so a stream opening or closing
    /// between them can't be clobbered by a stale status.
    pub async fn register_node(&self, mut node: Node) -> Result<(), StoreError> {
        let event_type = if let Some(ref client) = self.client {
            let _guard = self.node_op_lock.lock().await;
            let key = node_etcd_key(&node.name);
            let resp = client.clone().get(key.clone(), None).await?;
            let existing = resp.kvs().first();
            let is_new = existing.is_none();
            node.status = match existing.map(|kv| Node::decode(kv.value())) {
                Some(Ok(known)) => known.status,
                Some(Err(_)) => {
                    tracing::warn!(node = %node.name, "replacing undecodable node record");
                    NodeStatus::NotReady as i32
                }
                None => NodeStatus::NotReady as i32,
            };
            client.clone().put(key, node.encode_to_vec(), None).await?;
            if is_new {
                EventType::Added
            } else {
                EventType::Modified
            }
        } else {
            let mut nodes = self.nodes.write().await;
            node.status = match nodes.get(&node.name) {
                Some(known) => known.status,
                None => NodeStatus::NotReady as i32,
            };
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

    /// Inserts or replaces a node verbatim, status included, then publishes the resulting
    /// ADDED or MODIFIED event. Test-only seeding: registration goes through
    /// `register_node`, which is what keeps the API the sole writer of node status.
    #[cfg(test)]
    pub(crate) async fn upsert_and_publish_node(&self, node: Node) -> Result<(), StoreError> {
        let event_type = if let Some(ref client) = self.client {
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

    /// Records that a desired-state stream has opened for this node, marking it READY on
    /// the first one. Returns `false` if the store has never heard of the node, so the
    /// caller can tell the agent to register before watching.
    ///
    /// The count guard is held across the status write so the two can't disagree.
    pub async fn node_watch_started(&self, name: &str) -> Result<bool, StoreError> {
        // Checked before taking the lock so an unknown node doesn't queue behind every
        // other connect in the cluster. The count below is still the authority.
        if self.get_node(name).await?.is_none() {
            return Ok(false);
        }

        let mut watchers = self.node_watchers.lock().await;
        let current = watchers.get(name).copied().unwrap_or(0);
        if current == 0 {
            // A `false` here only means the status was already READY, which is the
            // ordinary case; existence was settled above.
            self.set_node_status(name, NodeStatus::Ready).await?;
        }
        watchers.insert(name.to_string(), current + 1);
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) async fn watcher_count(&self, name: &str) -> usize {
        self.node_watchers
            .lock()
            .await
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Records that a stream has closed, marking the node NOT_READY once the last one
    /// goes. Called from a `Drop`, so there is nowhere to return an error to.
    pub async fn node_watch_ended(&self, name: &str) {
        let mut watchers = self.node_watchers.lock().await;
        let Some(count) = watchers.get_mut(name) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count > 0 {
            return;
        }

        watchers.remove(name);
        if let Err(e) = self.set_node_status(name, NodeStatus::NotReady).await {
            tracing::warn!(
                node = %name,
                error = ?e,
                "failed to mark node NotReady after its last watch ended"
            );
        }
    }

    /// Sets the node's status and publishes the resulting MODIFIED event. Returns
    /// `false` if no node exists by that name, or if it already had that status, so a
    /// reconnecting agent doesn't republish an event that says nothing changed.
    ///
    /// Serialised against registration on the same node: the etcd branch shares
    /// `node_op_lock`, the in-memory branch shares the `nodes` write guard.
    pub async fn set_node_status(
        &self,
        name: &str,
        status: NodeStatus,
    ) -> Result<bool, StoreError> {
        let node = if let Some(ref client) = self.client {
            let _guard = self.node_op_lock.lock().await;
            let key = node_etcd_key(name);
            let resp = client.clone().get(key.clone(), None).await?;
            let Some(kv) = resp.kvs().first() else {
                return Ok(false);
            };
            // Skip rather than propagate: this surfaces on the agent's watch, and
            // failing it would lock that agent out permanently over one bad record.
            let Ok(mut node) = Node::decode(kv.value()) else {
                tracing::warn!(node = %name, "skipping undecodable node record");
                return Ok(false);
            };
            if node.status == status as i32 {
                return Ok(false);
            }
            node.status = status as i32;
            client.clone().put(key, node.encode_to_vec(), None).await?;
            node
        } else {
            let mut nodes = self.nodes.write().await;
            let Some(node) = nodes.get_mut(name) else {
                return Ok(false);
            };
            if node.status == status as i32 {
                return Ok(false);
            }
            node.status = status as i32;
            node.clone()
        };

        self.publish_node_event(WatchNodeEvent {
            event_type: EventType::Modified as i32,
            node: Some(node),
        });
        Ok(true)
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

    /// Snapshots pods that still need a placement decision (no `node_name`
    /// yet) and subscribes to future pod events, so a scheduler that just
    /// (re)connected sees pods that were already pending before it started
    /// watching — their one-time Added event fired before anyone was
    /// listening. In-memory mode subscribes before releasing the read lock:
    /// a write can't land between "read current state" and "start
    /// listening," since it needs that same lock to publish (see
    /// `update_and_publish_pod`).
    pub async fn subscribe_pod_events_with_snapshot(
        &self,
    ) -> Result<(Vec<PodDetail>, broadcast::Receiver<WatchPodEvent>), StoreError> {
        if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(
                    pods_prefix(),
                    Some(etcd_client::GetOptions::default().with_prefix()),
                )
                .await?;
            let mut pods: Vec<PodDetail> = resp
                .kvs()
                .iter()
                .filter_map(|kv| {
                    PodDetail::decode(kv.value())
                        .map_err(|e| {
                            let key = String::from_utf8_lossy(kv.key());
                            tracing::warn!(key = %key, error = %e, "skipping undecodable pod in pod-watch snapshot");
                        })
                        .ok()
                })
                .collect();
            pods.sort_by_key(|pod| pod.node_name.is_empty());
            let receiver = self.pod_events.subscribe();
            Ok((pods, receiver))
        } else {
            let stored = self.pods.read().await;
            let mut pods: Vec<PodDetail> = stored.values().cloned().collect();
            pods.sort_by_key(|pod| pod.node_name.is_empty());
            let receiver = self.pod_events.subscribe();
            Ok((pods, receiver))
        }
    }

    /// Snapshots every current node and subscribes to future node events, so
    /// a scheduler that just (re)connected has the full capacity picture
    /// immediately instead of waiting for the next node event. See
    /// `subscribe_pod_events_with_snapshot` for why in-memory mode holds the
    /// read lock across the subscribe call.
    pub async fn subscribe_node_events_with_snapshot(
        &self,
    ) -> Result<(Vec<Node>, broadcast::Receiver<WatchNodeEvent>), StoreError> {
        if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(
                    nodes_prefix(),
                    Some(etcd_client::GetOptions::default().with_prefix()),
                )
                .await?;
            let nodes = resp
                .kvs()
                .iter()
                .filter_map(|kv| {
                    Node::decode(kv.value())
                        .map_err(|e| {
                            let key = String::from_utf8_lossy(kv.key());
                            tracing::warn!(key = %key, error = %e, "skipping undecodable node in node-watch snapshot");
                        })
                        .ok()
                })
                .collect();
            let receiver = self.node_events.subscribe();
            Ok((nodes, receiver))
        } else {
            let nodes_guard = self.nodes.read().await;
            let nodes = nodes_guard.values().cloned().collect();
            let receiver = self.node_events.subscribe();
            Ok((nodes, receiver))
        }
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
    /// desired-state channel. In etcd mode, does a prefix scan; in-memory mode,
    /// reads the pods HashMap directly.
    pub async fn subscribe_desired_state_with_snapshot(
        &self,
        node_name: &str,
    ) -> Result<
        (
            Vec<PodWithSpec>,
            broadcast::Receiver<WatchDesiredStateEvent>,
        ),
        StoreError,
    > {
        let assigned = if let Some(ref client) = self.client {
            let resp = client
                .clone()
                .get(
                    pods_prefix(),
                    Some(etcd_client::GetOptions::default().with_prefix()),
                )
                .await?;
            resp.kvs()
                .iter()
                .filter_map(|kv| {
                    PodDetail::decode(kv.value())
                        .map_err(|e| {
                            let key = String::from_utf8_lossy(kv.key());
                            tracing::warn!(key = %key, error = %e, "skipping undecodable pod in desired-state snapshot");
                        })
                        .ok()
                })
                .filter(|pod| pod.node_name == node_name)
                .filter_map(|pod| pod.core)
                .collect()
        } else {
            let pods = self.pods.read().await;
            pods.values()
                .filter(|pod| pod.node_name == node_name)
                .filter_map(|pod| pod.core.clone())
                .collect()
        };
        let receiver = self.subscribe_desired_state_events(node_name).await;
        Ok((assigned, receiver))
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
    use proto::shared::v1::{NodeStatus, Resources};

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
            .await
            .unwrap();
        let mut events = store.subscribe_pod_events();

        let removed = store.remove_pod("default", "my-pod").await.unwrap();

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

        assert!(
            store
                .remove_pod("default", "ghost")
                .await
                .unwrap()
                .is_none()
        );
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
            .await
            .unwrap();
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
    async fn test_set_node_status_publishes_modified() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();
        let mut events = store.subscribe_node_events();

        assert!(
            store
                .set_node_status("node-1", NodeStatus::NotReady)
                .await
                .unwrap()
        );

        let event = events.try_recv().expect("a node event should be published");
        assert_eq!(event.event_type, EventType::Modified as i32);
        assert_eq!(event.node.unwrap().status, NodeStatus::NotReady as i32);
    }

    #[tokio::test]
    async fn test_set_node_status_is_a_noop_when_unchanged() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();
        let mut events = store.subscribe_node_events();

        assert!(
            !store
                .set_node_status("node-1", NodeStatus::Ready)
                .await
                .unwrap()
        );
        assert!(
            events.try_recv().is_err(),
            "an unchanged status must not publish an event"
        );
    }

    #[tokio::test]
    async fn test_set_node_status_unknown_node_is_a_noop() {
        let store = Store::new();
        assert!(
            !store
                .set_node_status("ghost", NodeStatus::NotReady)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_register_node_first_seen_is_not_ready() {
        let store = Store::new();
        let mut events = store.subscribe_node_events();

        store
            .register_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        let event = events.try_recv().expect("a node event should be published");
        assert_eq!(event.event_type, EventType::Added as i32);
        assert_eq!(
            event.node.unwrap().status,
            NodeStatus::NotReady as i32,
            "a node with no stream open isn't alive, whatever it reported"
        );
    }

    #[tokio::test]
    async fn test_register_node_preserves_ready_status() {
        let store = Store::new();
        // Stands in for the node's stream having opened.
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        store
            .register_node(test_support::node("node-1", NodeStatus::NotReady))
            .await
            .unwrap();

        assert_eq!(
            store.get_node("node-1").await.unwrap().map(|n| n.status),
            Some(NodeStatus::Ready as i32),
            "registration must not overwrite the status its own stream established"
        );
    }

    #[tokio::test]
    async fn test_register_node_preserves_not_ready_status() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await
            .unwrap();

        store
            .register_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        assert_eq!(
            store.get_node("node-1").await.unwrap().map(|n| n.status),
            Some(NodeStatus::NotReady as i32)
        );
    }

    #[tokio::test]
    async fn test_register_node_updates_capacity_and_allocatable() {
        let store = Store::new();
        store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await
            .unwrap();

        let mut reported = test_support::node("node-1", NodeStatus::NotReady);
        reported.capacity = Some(Resources {
            cpu: 4000,
            memory: 8192,
        });
        reported.allocatable = Some(Resources {
            cpu: 3500,
            memory: 7000,
        });
        store.register_node(reported).await.unwrap();

        let stored = store.get_node("node-1").await.unwrap().unwrap();
        assert_eq!(
            stored.capacity,
            Some(Resources {
                cpu: 4000,
                memory: 8192
            }),
            "the agent still owns the fields only it can observe"
        );
        assert_eq!(
            stored.allocatable,
            Some(Resources {
                cpu: 3500,
                memory: 7000
            })
        );
        assert_eq!(stored.status, NodeStatus::Ready as i32);
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
}
