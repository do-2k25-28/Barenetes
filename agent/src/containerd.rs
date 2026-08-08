//! Thin wrapper around the containerd gRPC API: pull an image, run a container
//! from it, and tear it down again.

use std::time::Duration;

use containerd_client::services::v1::{
    Container as CtrdContainer, CreateContainerRequest, CreateTaskRequest, DeleteContainerRequest,
    DeleteTaskRequest, GetImageRequest, KillRequest, ListContainersRequest, ReadContentRequest,
    StartRequest, TransferOptions, TransferRequest, WaitRequest,
    container::Runtime,
    containers_client::ContainersClient,
    content_client::ContentClient,
    images_client::ImagesClient,
    snapshots::snapshots_client::SnapshotsClient,
    snapshots::{PrepareSnapshotRequest, RemoveSnapshotRequest},
    tasks_client::TasksClient,
    transfer_client::TransferClient,
};
use containerd_client::types::Platform;
use containerd_client::types::transfer::{ImageStore, OciRegistry, UnpackConfiguration};
use containerd_client::{to_any, with_namespace};
use prost_types::Any;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tonic::transport::Channel;
use tonic::{Code, Request, Status};

/// All pods share one containerd namespace
const NAMESPACE: &str = "barenetes";
const SNAPSHOTTER: &str = "overlayfs";
const RUNTIME: &str = "io.containerd.runc.v2";
const POD_LABEL: &str = "barenetes.io/pod";
const SIGTERM: u32 = 15;
const SIGKILL: u32 = 9;

/// What we need from an image config to start a container.
struct ImageConfig {
    /// Chain ID of the unpacked layers, used as snapshot parent.
    parent: String,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
}

#[derive(Clone)]
pub struct Containerd {
    channel: Channel,
}

impl Containerd {
    pub async fn connect(socket: &str) -> Result<Self, tonic::transport::Error> {
        let channel = containerd_client::connect(socket).await?;
        Ok(Self { channel })
    }

    /// Pull `container.image` and start it as `<pod>-<container.name>`.
    pub async fn run_container(
        &self,
        pod: &str,
        container: &proto::shared::v1::Container,
    ) -> Result<(), Status> {
        let id = container_id(pod, &container.name);

        self.pull_image(&container.image).await?;
        let config = self.image_config(&container.image).await?;

        /// Writable layer on top of the image, mounted as the container rootfs.
        let mounts = SnapshotsClient::new(self.channel())
            .prepare(with_namespace!(
                PrepareSnapshotRequest {
                    snapshotter: SNAPSHOTTER.to_string(),
                    key: id.clone(),
                    parent: config.parent,
                    ..Default::default()
                },
                NAMESPACE
            ))
            .await?
            .into_inner()
            .mounts;

        let mut env = config.env;
        env.extend(
            container
                .env
                .iter()
                .map(|e| format!("{}={}", e.name, e.value)),
        );
        let spec = crate::oci::spec(&id, &config.args, &env, &config.cwd);

        ContainersClient::new(self.channel())
            .create(with_namespace!(
                CreateContainerRequest {
                    container: Some(CtrdContainer {
                        id: id.clone(),
                        image: container.image.clone(),
                        runtime: Some(Runtime {
                            name: RUNTIME.to_string(),
                            options: None,
                        }),
                        spec: Some(Any {
                            type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec"
                                .to_string(),
                            value: spec.to_string().into_bytes(),
                        }),
                        snapshotter: SNAPSHOTTER.to_string(),
                        snapshot_key: id.clone(),
                        labels: [(POD_LABEL.to_string(), pod.to_string())].into(),
                        ..Default::default()
                    }),
                },
                NAMESPACE
            ))
            .await?;

        let mut tasks = TasksClient::new(self.channel());
        // Empty stdio paths tell the shim to discard the container output.
        tasks
            .create(with_namespace!(
                CreateTaskRequest {
                    container_id: id.clone(),
                    rootfs: mounts,
                    ..Default::default()
                },
                NAMESPACE
            ))
            .await?;
        tasks
            .start(with_namespace!(
                StartRequest {
                    container_id: id.clone(),
                    ..Default::default()
                },
                NAMESPACE
            ))
            .await?;

        Ok(())
    }

    /// Stop and delete every container belonging to `pod`.
    /// Containers are first asked to stop with SIGTERM (SIGKILL when `force`),
    /// then killed for good if they are still alive after `grace_period`.
    pub async fn remove_pod(
        &self,
        pod: &str,
        grace_period: Duration,
        force: bool,
    ) -> Result<(), Status> {
        for id in self.pod_containers(pod).await? {
            let mut tasks = TasksClient::new(self.channel());

            let signal = if force { SIGKILL } else { SIGTERM };
            let killed = ignore_missing(
                tasks
                    .kill(with_namespace!(
                        KillRequest {
                            container_id: id.clone(),
                            signal,
                            all: true,
                            ..Default::default()
                        },
                        NAMESPACE
                    ))
                    .await,
            )?;

            if killed.is_some() {
                let wait = tasks.wait(with_namespace!(
                    WaitRequest {
                        container_id: id.clone(),
                        ..Default::default()
                    },
                    NAMESPACE
                ));
                if tokio::time::timeout(grace_period, wait).await.is_err() {
                    ignore_missing(
                        tasks
                            .kill(with_namespace!(
                                KillRequest {
                                    container_id: id.clone(),
                                    signal: SIGKILL,
                                    all: true,
                                    ..Default::default()
                                },
                                NAMESPACE
                            ))
                            .await,
                    )?;
                }

                ignore_missing(
                    tasks
                        .delete(with_namespace!(
                            DeleteTaskRequest {
                                container_id: id.clone(),
                            },
                            NAMESPACE
                        ))
                        .await,
                )?;
            }

            ignore_missing(
                ContainersClient::new(self.channel())
                    .delete(with_namespace!(
                        DeleteContainerRequest { id: id.clone() },
                        NAMESPACE
                    ))
                    .await,
            )?;

            ignore_missing(
                SnapshotsClient::new(self.channel())
                    .remove(with_namespace!(
                        RemoveSnapshotRequest {
                            snapshotter: SNAPSHOTTER.to_string(),
                            key: id.clone(),
                        },
                        NAMESPACE
                    ))
                    .await,
            )?;
        }

        Ok(())
    }

    /// Ids of the containers labelled as belonging to `pod`.
    async fn pod_containers(&self, pod: &str) -> Result<Vec<String>, Status> {
        let containers = ContainersClient::new(self.channel())
            .list(with_namespace!(
                ListContainersRequest {
                    filters: vec![format!(r#"labels."{POD_LABEL}"=="{pod}""#)],
                },
                NAMESPACE
            ))
            .await?
            .into_inner()
            .containers;

        Ok(containers.into_iter().map(|c| c.id).collect())
    }

    /// Pull the image into the content store and unpack it into the snapshotter.
    async fn pull_image(&self, image: &str) -> Result<(), Status> {
        let platform = platform();
        let source = OciRegistry {
            reference: image.to_string(),
            resolver: Default::default(),
        };
        let destination = ImageStore {
            name: image.to_string(),
            platforms: vec![platform.clone()],
            unpacks: vec![UnpackConfiguration {
                platform: Some(platform),
                snapshotter: SNAPSHOTTER.to_string(),
            }],
            ..Default::default()
        };

        TransferClient::new(self.channel())
            .transfer(with_namespace!(
                TransferRequest {
                    source: Some(to_any(&source)),
                    destination: Some(to_any(&destination)),
                    options: Some(TransferOptions::default()),
                },
                NAMESPACE
            ))
            .await?;

        Ok(())
    }

    /// Walk index of the image, manifest and config of a pulled image to find how to run it.
    async fn image_config(&self, image: &str) -> Result<ImageConfig, Status> {
        let target = ImagesClient::new(self.channel())
            .get(with_namespace!(
                GetImageRequest {
                    name: image.to_string(),
                },
                NAMESPACE
            ))
            .await?
            .into_inner()
            .image
            .and_then(|i| i.target)
            .ok_or_else(|| Status::not_found(format!("image {image} has no target")))?;

        let mut manifest = self.read_json(&target.digest).await?;

        /// Multi-platform images point at a list of manifests, one per platform.
        if let Some(manifests) = manifest["manifests"].as_array() {
            let platform = platform();
            let digest = manifests
                .iter()
                .find(|m| {
                    m["platform"]["os"] == platform.os
                        && m["platform"]["architecture"] == platform.architecture
                })
                .and_then(|m| m["digest"].as_str())
                .ok_or_else(|| {
                    Status::not_found(format!("image {image} has no {} manifest", platform.os))
                })?
                .to_string();
            manifest = self.read_json(&digest).await?;
        }

        let digest = manifest["config"]["digest"]
            .as_str()
            .ok_or_else(|| Status::internal(format!("image {image} has no config")))?
            .to_string();
        let config = self.read_json(&digest).await?;

        let diff_ids = strings(&config["rootfs"]["diff_ids"]);
        let mut args = strings(&config["config"]["Entrypoint"]);
        args.extend(strings(&config["config"]["Cmd"]));
        if args.is_empty() {
            return Err(Status::invalid_argument(format!(
                "image {image} has no entrypoint nor command"
            )));
        }

        Ok(ImageConfig {
            parent: chain_id(&diff_ids),
            args,
            env: strings(&config["config"]["Env"]),
            cwd: config["config"]["WorkingDir"]
                .as_str()
                .unwrap_or("/")
                .to_string(),
        })
    }

    /// Read a JSON blob (manifest) from the content store.
    async fn read_json(&self, digest: &str) -> Result<Value, Status> {
        let mut stream = ContentClient::new(self.channel())
            .read(with_namespace!(
                ReadContentRequest {
                    digest: digest.to_string(),
                    ..Default::default()
                },
                NAMESPACE
            ))
            .await?
            .into_inner();

        let mut blob = Vec::new();
        while let Some(chunk) = stream.message().await? {
            blob.extend_from_slice(&chunk.data);
        }

        serde_json::from_slice(&blob).map_err(|e| Status::internal(format!("invalid blob: {e}")))
    }

    fn channel(&self) -> Channel {
        self.channel.clone()
    }
}

fn container_id(pod: &str, container: &str) -> String {
    format!("{pod}-{container}")
}

fn platform() -> Platform {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => arch,
    };

    Platform {
        os: "linux".to_string(),
        architecture: architecture.to_string(),
        ..Default::default()
    }
}

/// Chain ID of a layer stack, the key under which containerd stores the unpacked image in the snapshotter.
fn chain_id(diff_ids: &[String]) -> String {
    let mut chain = diff_ids.first().cloned().unwrap_or_default();
    for diff_id in diff_ids.iter().skip(1) {
        let digest = Sha256::digest(format!("{chain} {diff_id}"));
        chain = format!("sha256:{digest:x}");
    }
    chain
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Deleting what is already gone is not an error.
fn ignore_missing<T>(result: Result<T, Status>) -> Result<Option<T>, Status> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(status) if status.code() == Code::NotFound => Ok(None),
        Err(status) => Err(status),
    }
}
