use proto::scheduler::v1::{SchedulePodRequest, SchedulePodResponse, scheduler_server::Scheduler};
use proto::shared::v1::{Node, NodeStatus, Resources};
use tonic::{Request, Response, Status};

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
*/
#[derive(Debug)]
pub struct BasicScheduler {
    nodes: Vec<Node>,
}

#[tonic::async_trait]
impl Scheduler for BasicScheduler {
    async fn schedule_pod(
        &self,
        request: Request<SchedulePodRequest>,
    ) -> Result<Response<SchedulePodResponse>, Status> {
        let pod = request
            .into_inner()
            .pod
            .ok_or(Status::invalid_argument("Missing pod"))?;

        println!("Finding a candidate for pod {}", pod.name);

        let resources = pod
            .limits
            .ok_or(Status::invalid_argument("Missing resources field"))?;

        println!(
            "Pod asks for {} mCPU and {} MB of RAM",
            resources.cpu, resources.memory
        );

        let candidates: Vec<&Node> = self
            .nodes
            .iter()
            // Don't schedule on nodes that aren't ready
            .filter(|node| node.status() == NodeStatus::Ready)
            // Only keep nodes that have the capacity to run the pod
            .filter(|node| node.capacity.is_some())
            .filter(|node| {
                node.allocatable.as_ref().is_some_and(|allocatable| {
                    allocatable.cpu > resources.cpu && allocatable.memory > resources.memory
                })
            })
            .collect();

        println!("Found candidates: {:?}", candidates);

        // Now that we have a list of candidates, we determine the node doing
        // the less amount of work by doing getting min( (ramUsage% + cpuUsage%)/2 )

        let mut elected = candidates
            .first()
            .ok_or(Status::resource_exhausted("No valid candidate found"))?;

        for candidate in candidates.iter() {
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

        println!("Elected {:?}", elected);

        Ok(Response::from(SchedulePodResponse {
            node_name: elected.name.clone(),
        }))
    }
}

// Random plausible data for testing purposes
// TODO: Use the API Server to get the actual node state
impl Default for BasicScheduler {
    fn default() -> Self {
        let nodes = vec![
            Node {
                name: String::from("Black Pearl"),
                status: NodeStatus::Ready.into(),
                capacity: Some(Resources {
                    cpu: 8000,
                    memory: 32768,
                }),
                allocatable: Some(Resources {
                    cpu: 7800,
                    memory: 31000,
                }),
            },
            Node {
                name: String::from("Flying Dutchman"),
                status: NodeStatus::Cordon.into(),
                capacity: Some(Resources {
                    cpu: 4000,
                    memory: 16384,
                }),
                allocatable: Some(Resources {
                    cpu: 3900,
                    memory: 15200,
                }),
            },
            Node {
                name: String::from("Davy Jones' Locker"),
                status: NodeStatus::Drain.into(),
                capacity: Some(Resources {
                    cpu: 16000,
                    memory: 65536,
                }),
                allocatable: Some(Resources {
                    cpu: 15800,
                    memory: 63000,
                }),
            },
            Node {
                name: String::from("Silent Mary"),
                status: NodeStatus::NotReady.into(),
                capacity: Some(Resources {
                    cpu: 2000,
                    memory: 8192,
                }),
                allocatable: Some(Resources {
                    cpu: 1900,
                    memory: 7000,
                }),
            },
            Node {
                name: String::from("Queen Anne's Revenge"),
                status: NodeStatus::Ready.into(),
                capacity: Some(Resources {
                    cpu: 4000,
                    memory: 16384,
                }),
                allocatable: Some(Resources {
                    cpu: 3500,
                    memory: 14000,
                }),
            },
        ];

        BasicScheduler { nodes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
