use anyhow::Context;
use clap::Parser;
use proto::agent::v1::kubelet_server::KubeletServer;
use proto::tls::{TlsArgs, TlsMode, load_server_tls_config, tls_mode};
use tonic::transport::Server;

mod cni;
mod containerd;
mod desired_state;
mod kubelet;
mod oci;
mod vlan;

const CONTAINERD_SOCKET: &str = "/run/containerd/containerd.sock";
const CNI_SOCKET: &str = "/run/barenetes/cni.sock";
const AGENT_STATE_DIR: &str = "/var/lib/barenetes/agent";

/// The agent runs on worker nodes, never on the control plane, so there's no
/// sensible default: the operator must always say where the API server is.
#[derive(Parser)]
#[command(
    name = "agent",
    version,
    about = "Barenetes agent (kubelet equivalent)"
)]
struct Cli {
    /// Address of the API server (e.g. http://127.0.0.1:50052)
    #[arg(long, env = "BARENETES_SERVER")]
    server: String,

    /// Address to bind the kubelet service on
    #[arg(long, env = "BARENETES_AGENT_ADDR", default_value = "127.0.0.1:50053")]
    addr: String,

    // Shared by both roles this binary plays: the Kubelet gRPC server
    // (mTLS server config) and the desired-state watch client of `api`
    // (mTLS client config, see desired_state::run).
    #[command(flatten)]
    tls: TlsArgs,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let addr = cli.addr.parse()?;

    let containerd = containerd::Containerd::connect(CONTAINERD_SOCKET).await?;
    let cni = cni::Cni::new(CNI_SOCKET);
    let vlans = vlan::VlanAllocations::new(AGENT_STATE_DIR)?;
    let kubelet = kubelet::KubeletService::new(containerd, cni, vlans);

    println!("Kubelet service starting on {addr}");

    let server_tls = cli.tls.clone();
    let server_task = tokio::spawn(async move {
        let mut server = Server::builder();
        match tls_mode(&server_tls)? {
            TlsMode::Mtls { cert, key, ca } => {
                server = server.tls_config(load_server_tls_config(&cert, &key, &ca)?)?;
                println!("mTLS enabled: client certificates are required");
            }
            TlsMode::Plaintext => {
                println!(
                    "starting kubelet service WITHOUT TLS: connections are unauthenticated and unencrypted (set --tls-cert/--tls-key/--tls-ca to enable mTLS)"
                );
            }
        }

        server
            .add_service(KubeletServer::new(kubelet))
            .serve(addr)
            .await
            .map_err(anyhow::Error::from)
    });

    let node_name = desired_state::detect_node_name()?;
    println!(
        "Connecting to API server at {} as node {node_name}",
        cli.server
    );
    let desired_state_task =
        tokio::spawn(desired_state::run(cli.server, node_name, cli.addr, cli.tls));

    // Both tasks are meant to run for the process' whole lifetime, so the
    // first one to finish did so because something broke. `try_join!` would
    // wait for *both*, which means a fatal desired-state error (a malformed
    // API server address, say) would sit unreported behind the kubelet server
    // running forever. Select instead, and let systemd restart us.
    tokio::select! {
        result = server_task => result.context("kubelet server task panicked")?,
        result = desired_state_task => result.context("desired-state task panicked")?,
    }
}
