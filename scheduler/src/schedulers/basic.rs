use std::collections::{HashMap, HashSet};

use proto::shared::v1::{Node, NodeStatus, Pod, Port, Resources};

type PodKey = (String, String);
/// (protocol, external port) — the pair that must be unique per node, since
/// that's what the node's CNI daemon binds on the host.
type PortKey = (i32, u32);

fn port_keys(ports: &[Port]) -> HashSet<PortKey> {
    ports
        .iter()
        .map(|port| (port.protocol, port.external))
        .collect()
}

/**
A very simple scheduler that schedules pods to the
node that is doing the less amount of work while
still having enough resources to fit the given pod
by only looking at the resource limits and requested host ports.

Node state is fed in from the API server's `WatchNodes` stream
(see `upsert_node`/`remove_node`) rather than hardcoded, so the
caller is responsible for keeping it current.
*/
#[derive(Debug, Default)]
pub struct BasicScheduler {
    nodes: HashMap<String, Node>,
    claimed: HashMap<String, Resources>,
    claimed_ports: HashMap<String, HashSet<PortKey>>,
    placements: HashMap<PodKey, (String, Resources, Vec<Port>)>,
}

impl BasicScheduler {
    pub fn upsert_node(&mut self, node: Node) {
        self.nodes.insert(node.name.clone(), node);
    }

    /// Drops `name` and its claimed/placement bookkeeping entirely — unlike
    /// `evict_node`, this does NOT return the dropped pods for rescheduling.
    /// Correct only because nothing today ever removes a node this way (no
    /// `Deleted` node event is published anywhere in the API server); if a
    /// node-deletion path is ever added, route it through `evict_node` plus a
    /// reschedule attempt instead of calling this directly, or pods on a
    /// deleted node will silently vanish from all scheduler bookkeeping.
    pub fn remove_node(&mut self, name: &str) {
        self.nodes.remove(name);
        self.claimed.remove(name);
        self.claimed_ports.remove(name);
        self.placements.retain(|_, (node, _, _)| node != name);
    }

    /// Whether `node_name` has been explicitly reported `NOT_READY`. An unseen
    /// node (no `WatchNodes` event has arrived for it yet) and a node in any
    /// other status (`READY`, `CORDON`, `DRAIN`) both return `false` — only a
    /// confirmed `NOT_READY` is grounds to distrust an already-recorded pod
    /// assignment to it. `CORDON`/`DRAIN` handling is out of scope here: a
    /// cordoned node's existing pods are still genuinely running there.
    pub fn is_confirmed_not_ready(&self, node_name: &str) -> bool {
        self.nodes
            .get(node_name)
            .is_some_and(|node| node.status() == NodeStatus::NotReady)
    }

    pub fn record_placement(
        &mut self,
        namespace: &str,
        name: &str,
        node_name: &str,
        limits: Resources,
        ports: &[Port],
    ) {
        let key = (namespace.to_string(), name.to_string());
        if let Some((existing_node, existing_limits, existing_ports)) = self.placements.get(&key) {
            if existing_node == node_name && *existing_limits == limits && existing_ports == ports {
                return;
            }
            self.release_placement(namespace, name);
        }
        let claimed = self.claimed.entry(node_name.to_string()).or_default();
        claimed.cpu += limits.cpu;
        claimed.memory += limits.memory;
        self.claimed_ports
            .entry(node_name.to_string())
            .or_default()
            .extend(port_keys(ports));
        self.placements
            .insert(key, (node_name.to_string(), limits, ports.to_vec()));
    }

    /// Returns whether a placement actually existed and was released.
    pub fn release_placement(&mut self, namespace: &str, name: &str) -> bool {
        let key = (namespace.to_string(), name.to_string());
        let Some((node_name, limits, ports)) = self.placements.remove(&key) else {
            return false;
        };
        if let Some(claimed) = self.claimed.get_mut(&node_name) {
            claimed.cpu -= limits.cpu;
            claimed.memory -= limits.memory;
        }
        if let Some(claimed_ports) = self.claimed_ports.get_mut(&node_name) {
            for port_key in port_keys(&ports) {
                claimed_ports.remove(&port_key);
            }
        }
        true
    }

    /// Releases every placement recorded against `node_name` — meant to be
    /// called once that node is reported `NOT_READY`, before the caller retries
    /// placement for each returned pod on a different node. Returns
    /// `(namespace, name, limits, ports)` for every pod that was evicted.
    pub fn evict_node(&mut self, node_name: &str) -> Vec<(String, String, Resources, Vec<Port>)> {
        let evicted: Vec<(String, String, Resources, Vec<Port>)> = self
            .placements
            .iter()
            .filter(|(_, (node, _, _))| node == node_name)
            .map(|((namespace, name), (_, limits, ports))| {
                (namespace.clone(), name.clone(), *limits, ports.clone())
            })
            .collect();

        for (namespace, name, _, _) in &evicted {
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

    /// Whether none of `ports` collide with a port already claimed on `node`.
    fn ports_available(&self, node: &Node, ports: &[Port]) -> bool {
        let Some(claimed) = self.claimed_ports.get(&node.name) else {
            return true;
        };
        port_keys(ports).is_disjoint(claimed)
    }

    /// Finds the node doing the least amount of work that still has
    /// enough capacity and free host ports to fit `pod`.
    ///
    /// Returns the elected node's name, or a human-readable reason
    /// the pod couldn't be placed (meant to be reported back via
    /// `AssignPod`'s `unschedulable_reason`).
    pub fn place(&self, pod: &Pod, ports: &[Port]) -> Result<String, String> {
        let resources = pod
            .limits
            .as_ref()
            .ok_or_else(|| "pod is missing a resources.limits field".to_string())?;

        let ready: Vec<&Node> = self
            .nodes
            .values()
            .filter(|node| node.status() == NodeStatus::Ready)
            .collect();
        if ready.is_empty() {
            return Err("no ready node available".to_string());
        }

        let with_capacity: Vec<&Node> = ready
            .into_iter()
            .filter(|node| {
                self.effective_allocatable(node).is_some_and(|allocatable| {
                    allocatable.cpu > resources.cpu && allocatable.memory > resources.memory
                })
            })
            .collect();
        if with_capacity.is_empty() {
            return Err("no ready node has enough capacity for this pod".to_string());
        }

        let candidates: Vec<&Node> = with_capacity
            .into_iter()
            .filter(|node| self.ports_available(node, ports))
            .collect();

        // Now that we have a list of candidates, we determine the node doing
        // the less amount of work by doing getting min( (ramUsage% + cpuUsage%)/2 )

        let mut elected = *candidates
            .first()
            .ok_or_else(|| "no ready node has a free host port for this pod".to_string())?;

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
    use proto::shared::v1::Protocol;

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

    fn tcp_port(external: u32) -> Port {
        Port {
            internal: external,
            external,
            protocol: Protocol::Tcp as i32,
        }
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

        scheduler.record_placement("default", "web", "n", limits(300, 200), &[]);

        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(700, 800))
        );
    }

    #[test]
    fn record_placement_is_idempotent_for_the_same_pod_and_node() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        scheduler.record_placement("default", "web", "n", limits(300, 200), &[]);
        scheduler.record_placement("default", "web", "n", limits(300, 200), &[]);

        assert_eq!(
            scheduler.effective_allocatable(&node),
            Some(limits(700, 800))
        );
    }

    #[test]
    fn release_placement_restores_effective_allocatable() {
        let node = get_node("n", limits(1000, 1000), limits(1000, 1000));
        let mut scheduler = BasicScheduler::default();

        scheduler.record_placement("default", "web", "n", limits(300, 200), &[]);

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
        scheduler.record_placement("default", "already-there", "full", limits(900, 900), &[]);

        let placed = scheduler
            .place(
                &Pod {
                    name: "new".to_string(),
                    status: 0,
                    requests: None,
                    limits: Some(limits(200, 200)),
                },
                &[],
            )
            .unwrap();

        assert_eq!(placed, "empty");
    }

    #[test]
    fn evict_node_releases_claims_and_returns_evicted_pods() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("dead", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement("default", "web", "dead", limits(300, 200), &[]);
        scheduler.record_placement("ns2", "other", "dead", limits(100, 100), &[]);
        scheduler.record_placement("default", "elsewhere", "alive", limits(50, 50), &[]);

        let evicted = scheduler.evict_node("dead");

        assert_eq!(evicted.len(), 2);
        assert!(evicted.contains(&(
            "default".to_string(),
            "web".to_string(),
            limits(300, 200),
            Vec::new()
        )));
        assert!(evicted.contains(&(
            "ns2".to_string(),
            "other".to_string(),
            limits(100, 100),
            Vec::new()
        )));

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
        scheduler.record_placement("default", "web", "gone", limits(300, 200), &[]);

        scheduler.remove_node("gone");

        // A same-named node reappearing later must not inherit the stale claim.
        scheduler.upsert_node(get_node("gone", limits(1000, 1000), limits(1000, 1000)));
        assert_eq!(
            scheduler.effective_allocatable(&get_node(
                "gone",
                limits(1000, 1000),
                limits(1000, 1000)
            )),
            Some(limits(1000, 1000))
        );
        assert!(!scheduler.release_placement("default", "web"));
    }

    #[test]
    fn is_confirmed_not_ready_true_only_for_a_known_not_ready_node() {
        let mut scheduler = BasicScheduler::default();
        assert!(!scheduler.is_confirmed_not_ready("unseen"));

        scheduler.upsert_node(get_node(
            "ready-node",
            limits(1000, 1000),
            limits(1000, 1000),
        ));
        assert!(!scheduler.is_confirmed_not_ready("ready-node"));

        let mut dead = get_node("dead-node", limits(1000, 1000), limits(1000, 1000));
        dead.status = NodeStatus::NotReady.into();
        scheduler.upsert_node(dead);
        assert!(scheduler.is_confirmed_not_ready("dead-node"));
    }

    fn pod(name: &str, cpu: i32, memory: i32) -> Pod {
        Pod {
            name: name.to_string(),
            status: 0,
            requests: None,
            limits: Some(limits(cpu, memory)),
        }
    }

    #[test]
    fn place_rejects_the_only_node_when_its_requested_port_is_already_claimed_there() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("only", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement(
            "default",
            "already-there",
            "only",
            limits(100, 100),
            &[tcp_port(8080)],
        );

        let err = scheduler
            .place(&pod("new", 100, 100), &[tcp_port(8080)])
            .unwrap_err();

        assert_eq!(err, "no ready node has a free host port for this pod");
    }

    #[test]
    fn place_skips_a_node_whose_port_is_taken_in_favor_of_a_free_one() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("taken", limits(1000, 1000), limits(1000, 1000)));
        scheduler.upsert_node(get_node("free", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement(
            "default",
            "already-there",
            "taken",
            limits(100, 100),
            &[tcp_port(8080)],
        );

        let placed = scheduler
            .place(&pod("new", 100, 100), &[tcp_port(8080)])
            .unwrap();

        assert_eq!(placed, "free");
    }

    #[test]
    fn releasing_the_conflicting_placement_frees_the_port_for_retry() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("only", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement(
            "default",
            "already-there",
            "only",
            limits(100, 100),
            &[tcp_port(8080)],
        );
        assert!(
            scheduler
                .place(&pod("new", 100, 100), &[tcp_port(8080)])
                .is_err()
        );

        assert!(scheduler.release_placement("default", "already-there"));

        let placed = scheduler
            .place(&pod("new", 100, 100), &[tcp_port(8080)])
            .unwrap();
        assert_eq!(placed, "only");
    }

    #[test]
    fn evict_node_returns_ports_so_a_reschedule_can_carry_the_constraint_over() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("dead", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement(
            "default",
            "web",
            "dead",
            limits(300, 200),
            &[tcp_port(8080)],
        );

        let evicted = scheduler.evict_node("dead");

        assert_eq!(
            evicted,
            vec![(
                "default".to_string(),
                "web".to_string(),
                limits(300, 200),
                vec![tcp_port(8080)]
            )]
        );
    }

    #[test]
    fn a_different_port_on_the_same_node_does_not_conflict() {
        let mut scheduler = BasicScheduler::default();
        scheduler.upsert_node(get_node("only", limits(1000, 1000), limits(1000, 1000)));
        scheduler.record_placement(
            "default",
            "already-there",
            "only",
            limits(100, 100),
            &[tcp_port(8080)],
        );

        let placed = scheduler
            .place(&pod("new", 100, 100), &[tcp_port(9090)])
            .unwrap();

        assert_eq!(placed, "only");
    }
}
