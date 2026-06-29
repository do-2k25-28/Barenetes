use tonic::{Request, Response, Status, transport::Server};

use proto::scheduler::v1::scheduler_server::{Scheduler, SchedulerServer};
use proto::scheduler::v1::{SchedulePodRequest, SchedulePodResponse};

#[derive(Debug, Default)]
pub struct MyScheduler {}

#[tonic::async_trait]
impl Scheduler for MyScheduler {
    async fn schedule_pod(
        &self,
        request: Request<SchedulePodRequest>,
    ) -> Result<Response<SchedulePodResponse>, Status> {
        let req = request.into_inner();
        let pod_name = req
            .pod
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "unknown".to_string());

        println!("Received SchedulePodRequest for pod: {}", pod_name);

        // Simple mock scheduling logic
        let response = SchedulePodResponse {
            node_name: "mock-node".to_string(),
        };

        Ok(Response::new(response))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50051".parse()?;
    let scheduler = MyScheduler::default();

    println!("Scheduler service starting on {}", addr);

    Server::builder()
        .add_service(SchedulerServer::new(scheduler))
        .serve(addr)
        .await?;

    Ok(())
}
