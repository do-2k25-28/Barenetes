fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    prost_config.type_attribute(
        ".cni.v1.PortMapping",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );

    tonic_prost_build::configure().compile_with_config(
        prost_config,
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
