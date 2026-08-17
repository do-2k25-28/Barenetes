use std::collections::HashMap;

use proto::shared::v1::{Node, PodDetail};
use tokio::sync::RwLock;

/// TODO : currently in-memory only for now, will need replacing with a database
/// (etcd may be overkill for the minimal version we are trying to acheive
/// so could look at some alternatives)
/// The api-server's state
///
/// Pods are keyed by (namespace, name).
/// Nodes are keyed by name.
#[derive(Default)]
pub struct Store {
    pods: RwLock<HashMap<(String, String), PodDetail>>,
    nodes: RwLock<HashMap<String, Node>>,
}

impl Store {
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
