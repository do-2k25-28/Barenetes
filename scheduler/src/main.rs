use tonic::transport::Server;

use proto::scheduler::v1::scheduler_server::SchedulerServer;

mod schedulers;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50051".parse()?;

    let scheduler = schedulers::BasicScheduler::default();

    println!("Scheduler service starting on {}", addr);

    Server::builder()
        .add_service(SchedulerServer::new(scheduler))
        .serve(addr)
        .await?;

    Ok(())
}
