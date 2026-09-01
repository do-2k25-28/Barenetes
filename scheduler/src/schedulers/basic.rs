use std::collections::HashMap;

use proto::shared::v1::{Node, NodeStatus, Pod};

/// Calculate the general usage of the given node.
/// Does it by averaging the cpu and memory usage.
///
/// Ex: 66% cpu usage with 33% memory usage ≈ 50% general usage
///
/// Returns `None` if the node is missing capacity or allocatable data,
/// which can happen with bad data coming over the network.
fn calculate_general_usage(node: &Node) -> Option<f32> {
    let capacity = node.capacity.as_ref()?;
    let allocatable = node.allocatable.as_ref()?;

    let cpu = (capacity.cpu as f32 - allocatable.cpu as f32) / capacity.cpu as f32;

    let memory = (capacity.memory as f32 - allocatable.memory as f32) / capacity.memory as f32;

    Some((cpu + memory) / 2.0)
}

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
}

impl BasicScheduler {
    pub fn upsert_node(&mut self, node: Node) {
        self.nodes.insert(node.name.clone(), node);
    }

    pub fn remove_node(&mut self, name: &str) {
        self.nodes.remove(name);
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
                node.allocatable.as_ref().is_some_and(|allocatable| {
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
                calculate_general_usage(elected),
                calculate_general_usage(candidate),
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
    use proto::shared::v1::Resources;

    fn get_node(capacity: Resources, allocatable: Resources) -> Node {
        Node {
            name: String::from(""),
            status: NodeStatus::Ready.into(),
            capacity: Some(capacity),
            allocatable: Some(allocatable),
        }
    }

    #[test]
    fn test_calculate_general_usage_0() {
        let node = get_node(
            Resources {
                cpu: 1000,
                memory: 1000,
            },
            Resources {
                cpu: 1000,
                memory: 1000,
            },
        );

        assert_eq!(calculate_general_usage(&node), Some(0.0));
    }

    #[test]
    fn test_calculate_general_usage_50() {
        let node = get_node(
            Resources {
                cpu: 1000,
                memory: 1000,
            },
            Resources {
                cpu: 500,
                memory: 500,
            },
        );
        // Half the CPU is used and half the memory is used
        // therefore general usage should be 0.5 (50%)
        assert_eq!(calculate_general_usage(&node), Some(0.5));
    }

    #[test]
    fn test_calculate_general_usage_100() {
        let node = get_node(
            Resources {
                cpu: 1000,
                memory: 1000,
            },
            Resources { cpu: 0, memory: 0 },
        );

        assert_eq!(calculate_general_usage(&node), Some(1.0));
    }
}
