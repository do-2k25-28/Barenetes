fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    prost_config.type_attribute(
        ".shared.v1.Port",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    prost_config.type_attribute(
        ".shared.v1.Protocol",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );

    tonic_prost_build::configure().compile_with_config(
        prost_config,
        &[
            "agent/v1/kubelet.proto",
            "api/v1/api.proto",
            "cni/v1/cni.proto",
            "shared/v1/container.proto",
            "shared/v1/node.proto",
            "shared/v1/pod.proto",
            "shared/v1/resources.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
