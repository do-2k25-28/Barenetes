use proto::scheduler::v1::{SchedulePodRequest, SchedulePodResponse, scheduler_server::Scheduler};
use proto::shared::v1::Resources;
use tonic::{Request, Response, Status};

// Temporary redefinition of what's in shared/v1/node.proto
// because it's not compiled by tonic as it's not currently used.

#[derive(Debug, PartialEq)]
pub enum NodeStatus {
    Ready,
    Cordon,
    Drain,
    NotReady,
}

#[derive(Debug)]
pub struct Node {
    name: String,
    status: NodeStatus,
    capacity: Resources,
    allocatable: Resources,
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
            .filter(|node| matches!(node.status, NodeStatus::Ready))
            // Only keep nodes that have the capacity to run the pod
            .filter(|node| {
                node.allocatable.cpu > resources.cpu && node.allocatable.memory > resources.memory
            })
            .collect();

        println!("Found candidates: {:?}", candidates);

        // Now that we have a list of candidates, we determine the node doing
        // the less amount of work by doing getting min( (ramUsage% + cpuUsage%)/2 )

        fn calculate_general_usage(node: &Node) -> f32 {
            let cpu =
                (node.capacity.cpu as f32 - node.allocatable.cpu as f32) / node.capacity.cpu as f32;

            let memory = (node.capacity.memory as f32 - node.allocatable.memory as f32)
                / node.capacity.memory as f32;

            (cpu + memory) / 2.0
        }

        let mut elected = candidates
            .first()
            .ok_or(Status::resource_exhausted("No valid candidate found"))?;

        for candidate in candidates.iter() {
            if calculate_general_usage(elected) > calculate_general_usage(candidate) {
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
                status: NodeStatus::Ready,
                capacity: Resources {
                    cpu: 8000,
                    memory: 32768,
                },
                allocatable: Resources {
                    cpu: 7800,
                    memory: 31000,
                },
            },
            Node {
                name: String::from("Flying Dutchman"),
                status: NodeStatus::Cordon,
                capacity: Resources {
                    cpu: 4000,
                    memory: 16384,
                },
                allocatable: Resources {
                    cpu: 3900,
                    memory: 15200,
                },
            },
            Node {
                name: String::from("Davy Jones' Locker"),
                status: NodeStatus::Drain,
                capacity: Resources {
                    cpu: 16000,
                    memory: 65536,
                },
                allocatable: Resources {
                    cpu: 15800,
                    memory: 63000,
                },
            },
            Node {
                name: String::from("Silent Mary"),
                status: NodeStatus::NotReady,
                capacity: Resources {
                    cpu: 2000,
                    memory: 8192,
                },
                allocatable: Resources {
                    cpu: 1900,
                    memory: 7000,
                },
            },
            Node {
                name: String::from("Queen Anne's Revenge"),
                status: NodeStatus::Ready,
                capacity: Resources {
                    cpu: 4000,
                    memory: 16384,
                },
                allocatable: Resources {
                    cpu: 3500,
                    memory: 14000,
                },
            },
        ];

        BasicScheduler { nodes }
    }
}
