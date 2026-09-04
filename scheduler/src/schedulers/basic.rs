use std::collections::HashMap;

use proto::shared::v1::{Node, NodeStatus, Pod, Resources};

type PodKey = (String, String);

/**
A very simple scheduler that schedules pods to the
node that is doing the less amount of work while
still having enough resources to fit the given pod
by only looking at the resource limits.

Node state is fed in from the API server's `WatchNodes` stream
(see `upsert_node`/`remove_node`) rather than hardcoded, so the
caller is responsible for keeping it current.
*/
#[derive(Debug, Default)]
pub struct BasicScheduler {
    nodes: HashMap<String, Node>,
    claimed: HashMap<String, Resources>,
    placements: HashMap<PodKey, (String, Resources)>,
}

impl BasicScheduler {
    pub fn upsert_node(&mut self, node: Node) {
        self.nodes.insert(node.name.clone(), node);
    }

    pub fn remove_node(&mut self, name: &str) {
        self.nodes.remove(name);
        self.claimed.remove(name);
        self.placements.retain(|_, (node, _)| node != name);
    }

    pub fn record_placement(
        &mut self,
        namespace: &str,
        name: &str,
        node_name: &str,
        limits: Resources,
    ) {
        let key = (namespace.to_string(), name.to_string());
        if let Some((existing_node, existing_limits)) = self.placements.get(&key) {
            if existing_node == node_name && *existing_limits == limits {
                return;
            }
            self.release_placement(namespace, name);
        }
        let claimed = self.claimed.entry(node_name.to_string()).or_default();
        claimed.cpu += limits.cpu;
        claimed.memory += limits.memory;
        self.placements.insert(key, (node_name.to_string(), limits));
    }

    /// Returns whether a placement actually existed and was released.
    pub fn release_placement(&mut self, namespace: &str, name: &str) -> bool {
        let key = (namespace.to_string(), name.to_string());
        let Some((node_name, limits)) = self.placements.remove(&key) else {
            return false;
        };
        if let Some(claimed) = self.claimed.get_mut(&node_name) {
            claimed.cpu -= limits.cpu;
            claimed.memory -= limits.memory;
        }
        true
    }

    /// Releases every placement recorded against `node_name` — meant to be
    /// called once that node is reported `NOT_READY`, before the caller retries
    /// placement for each returned pod on a different node. Returns
    /// `(namespace, name, limits)` for every pod that was evicted.
    pub fn evict_node(&mut self, node_name: &str) -> Vec<(String, String, Resources)> {
        let evicted: Vec<(String, String, Resources)> = self
            .placements
            .iter()
            .filter(|(_, (node, _))| node == node_name)
            .map(|((namespace, name), (_, limits))| (namespace.clone(), name.clone(), *limits))
            .collect();

        for (namespace, name, _) in &evicted {
            self.release_placement(namespace, name);
        }

        evicted
    }

    fn effective_allocatable(&self, node: &Node) -> Option<Resources> {
        let allocatable = node.allocatable?;
        let claimed = self.claimed.get(&node.name).copied().unwrap_or_default();
        Some(Resources {
            cpu: (allocatable.cpu - claimed.cpu).max(0),
            memory: (allocatable.memory - claimed.memory).max(0),
        })
    }

    /// Calculate the general usage of the given node.
    /// Does it by averaging the cpu and memory usage.
    ///
    /// Ex: 66% cpu usage with 33% memory usage ≈ 50% general usage
    ///
    /// Returns `None` if the node is missing capacity or allocatable data,
    /// which can happen with bad data coming over the network.
    fn calculate_general_usage(&self, node: &Node) -> Option<f32> {
        let capacity = node.capacity.as_ref()?;
        let allocatable = self.effective_allocatable(node)?;

        let cpu = (capacity.cpu as f32 - allocatable.cpu as f32) / capacity.cpu as f32;

        let memory = (capacity.memory as f32 - allocatable.memory as f32) / capacity.memory as f32;

        Some((cpu + memory) / 2.0)
    }

    /// Finds the node doing the least amount of work that still has
    /// enough capacity to fit `pod`.
    ///
    /// Returns the elected node's name, or a human-readable reason
    /// the pod couldn't be placed (meant to be reported back via
    /// `AssignPod`'s `unschedulable_reason`).
    pub fn place(&self, pod: &Pod) -> Result<String, String> {
        let resources = pod
            .limits
            .as_ref()
            .ok_or_else(|| "pod is missing a resources.limits field".to_string())?;

        let candidates: Vec<&Node> = self
            .nodes
            .values()
            // Don't schedule on nodes that aren't ready
            .filter(|node| node.status() == NodeStatus::Ready)
            // Only keep nodes that have the capacity to run the pod
            .filter(|node| {
                self.effective_allocatable(node).is_some_and(|allocatable| {
                    allocatable.cpu > resources.cpu && allocatable.memory > resources.memory
                })
            })
            .collect();

        // Now that we have a list of candidates, we determine the node doing
        // the less amount of work by doing getting min( (ramUsage% + cpuUsage%)/2 )

        let mut elected = *candidates
            .first()
            .ok_or_else(|| "no ready node has enough capacity for this pod".to_string())?;

        for candidate in &candidates {
            let (Some(elected_usage), Some(candidate_usage)) = (
                self.calculate_general_usage(elected),
                self.calculate_general_usage(candidate),
            ) else {
                // Silently ignore nodes with malformed capacity/allocatable data
                // instead of panicking on bad data coming over the network.
                continue;
            };

            if elected_usage > candidate_usage {
                elected = candidate;
            }
        }

        Ok(elected.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_node(name: &str, capacity: Resources, allocatable: Resources) -> Node {
        Node {
            name: name.to_string(),
            status: NodeStatus::Ready.into(),
            capacity: Some(capacity),
            allocatable: Some(allocatable),
        }
    }

    fn limits(cpu: i32, memory: i32) -> Resources {
        Resources { cpu, memory }
    }

    #[test]
    fn test_calculate_general_usage_0() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let scheduler = BasicScheduler::default();

        assert_eq!(scheduler.calculate_general_usage(&node), Some(0.0));
    }

    #[test]
    fn test_calculate_general_usage_50() {
        let node = get_node("n", limits(1000, 1000), limits(500, 500));
        let scheduler = BasicScheduler::default();
        // Half the CPU is used and half the memory is used
        // therefore general usage should be 0.5 (50%)
        assert_eq!(scheduler.calculate_general_usage(&node), Some(0.5));
    }

    #[test]
    fn test_calculate_general_usage_100() {
        let node = get_node("n", limits(1000, 1000), limits(0, 0));
        let scheduler = BasicScheduler::default();

        assert_eq!(scheduler.calculate_general_usage(&node), Some(1.0));
    }

    #[test]
    fn record_placement_reduces_effective_allocatable() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        scheduler.record_placement("default", "web", "n", limits(300, 200));

        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(700, 800))
        );
    }

    #[test]
    fn record_placement_is_idempotent_for_the_same_pod_and_node() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        scheduler.record_placement("default", "web", "n", limits(300, 200));
        scheduler.record_placement("default", "web", "n", limits(300, 200));

        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(700, 800))
        );
    }

    #[test]
    fn release_placement_restores_effective_allocatable() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        scheduler.record_placement("default", "web", "n", limits(300, 200));

        assert!(scheduler.release_placement("default", "web"));
        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(1000, 1000))
        );
    }

    #[test]
    fn release_placement_of_an_unplaced_pod_is_a_no_op() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        assert!(!scheduler.release_placement("default", "never-placed"));
        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(1000, 1000))
        );
    }

    #[test]
    fn place_skips_a_node_left_with_no_room_by_earlier_placements() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("full", limits(1000, 1000), limits(1000, 1000)));
        scheduler.upsert_node(get_node("empty", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement("default", "already-there", "full", limits(900, 900));

        let placed = scheduler
            .place(&Pod {
                name: "new".to_string(),
                status: 0,
                requests: None,
                limits: Some(limits(200, 200)),
            })
            .unwrap();

        assert_eq!(placed, "empty");
    }

    #[test]
    fn evict_node_releases_claims_and_returns_evicted_pods() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("dead", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement("default", "web", "dead", limits(300, 200));
        scheduler.record_placement("ns2", "other", "dead", limits(100, 100));
        scheduler.record_placement("default", "elsewhere", "alive", limits(50, 50));

        let evicted = scheduler.evict_node("dead");

        assert_eq!(evicted.len(), 2);
        assert!(
            evicted.contains(&("default".to_string(), "web".to_string(), limits(300, 200)))
        );
        assert!(
            evicted.contains(&("ns2".to_string(), "other".to_string(), limits(100, 100)))
        );

        let dead = get_node("dead", limits(1000, 1000), limits(1000, 1000));
        assert_eq!(
            scheduler.effective_allocatable(&dead),
            Some(limits(1000, 1000)),
            "claims on the evicted node must be released"
        );
        // The unrelated placement on "alive" must survive the eviction.
        assert!(scheduler.release_placement("default", "elsewhere"));
    }

    #[test]
    fn evict_node_with_no_placements_returns_empty() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("solo", limits(1000, 1000), limits(1000, 1000)));

        assert_eq!(scheduler.evict_node("solo"), Vec::new());
    }

    #[test]
    fn remove_node_drops_its_placements() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("gone", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement("default", "web", "gone", limits(300, 200));

        scheduler.remove_node("gone");

        // A same-named node reappearing later must not inherit the stale claim.
        scheduler.upsert_node(get_node("gone", limits(1000, 1000), limits(1000, 1000)));
        assert_eq!(
            scheduler.effective_allocatable(&get_node("gone", limits(1000, 1000), limits(1000, 1000))),
            Some(limits(1000, 1000))
        );
        assert!(!scheduler.release_placement("default", "web"));
    }
}
