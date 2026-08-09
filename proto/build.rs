fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure().compile_protos(
        &[
            "agent/v1/kubelet.proto",
            "api/v1/api.proto",
            "barectl/v1/kubectl.proto",
            "cni/v1/cni.proto",
            "scheduler/v1/scheduler.proto",
            "shared/v1/container.proto",
            "shared/v1/node.proto",
            "shared/v1/pod.proto",
            "shared/v1/resources.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
