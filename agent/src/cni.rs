//! Client for the local CNI daemon: plumbs containers into their tenant
//! network and tears the plumbing down again.
//!
//! The daemon exposes `proto::cni::v1::CniService` on a unix socket and does
//! the actual veth/VLAN/IPAM work; this module only builds requests.

use std::path::PathBuf;

use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, NetworkRef, WorkloadNetwork,
    WorkloadRef, cni_service_client::CniServiceClient,
};
use tonic::transport::Channel;
use tonic::{Request, Status};

#[derive(Clone)]
pub struct Cni {
    socket: PathBuf,
}

impl Cni {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Connect to the daemon. Reconnecting for every call keeps the agent
    /// working across CNI restarts at the cost of one unix connect.
    async fn client(&self) -> Result<CniServiceClient<Channel>, Status> {
        let channel = containerd_client::connect(&self.socket)
            .await
            .map_err(|error| Status::unavailable(format!("cannot reach CNI daemon: {error}")))?;
        Ok(CniServiceClient::new(channel))
    }

    /// Attach the network namespace of `pid` to its tenant network and return
    /// the resulting addressing information.
    pub async fn add_network(
        &self,
        workload: WorkloadRef,
        network: NetworkRef,
        pid: u32,
        interface_name: &str,
    ) -> Result<WorkloadNetwork, Status> {
        let response = self
            .client()
            .await?
            .add_workload_network(Request::new(AddWorkloadNetworkRequest {
                workload: Some(workload),
                network: Some(network),
                netns_path: format!("/proc/{pid}/ns/net"),
                interface_name: interface_name.to_string(),
                port_mappings: Vec::new(),
            }))
            .await?
            .into_inner();

        response
            .network
            .ok_or_else(|| Status::internal("CNI returned no network"))
    }

    /// Detach a workload from its tenant network.
    pub async fn delete_network(
        &self,
        workload: WorkloadRef,
        network: NetworkRef,
    ) -> Result<(), Status> {
        let response = self
            .client()
            .await?
            .delete_workload_network(Request::new(DeleteWorkloadNetworkRequest {
                workload: Some(workload),
                network: Some(network),
            }))
            .await?
            .into_inner();

        if !response.success {
            return Err(Status::internal("CNI failed to delete workload network"));
        }
        Ok(())
    }
}
