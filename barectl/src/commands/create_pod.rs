use proto::api::v1::CreatePodRequest;
use proto::shared::v1::PodWithSpec;

use crate::cli::CreatePodArgs;
use crate::client;
use crate::error::CliError;
use crate::manifest::PodManifest;

pub async fn run(server: &str, args: CreatePodArgs) -> Result<(), CliError> {
    let manifest = PodManifest::from_file(&args.file)?;
    let pod = PodWithSpec::try_from(manifest)?;

    let name = pod.pod.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let namespace = pod
        .spec
        .as_ref()
        .map(|s| s.namespace.clone())
        .unwrap_or_default();

    let mut client = client::connect(server).await?;
    client
        .create_pod(CreatePodRequest { pod: Some(pod) })
        .await?;

    println!("pod/{name} created in namespace \"{namespace}\"");
    Ok(())
}
