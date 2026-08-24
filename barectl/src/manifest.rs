use std::path::Path;

use proto::shared::v1::{
    Container, EnvVar, Pod, PodSpec, PodStatus, PodWithSpec, Port, Protocol, Resources,
};
use serde::Deserialize;

use crate::error::CliError;

/// A kubectl-style YAML manifest describing a single pod, as written by the user.
#[derive(Debug, Deserialize)]
pub struct PodManifest {
    #[serde(rename = "apiVersion")]
    #[allow(dead_code)]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: PodManifestSpec,
}

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub name: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Deserialize)]
pub struct PodManifestSpec {
    pub containers: Vec<ContainerManifest>,
    /// Aggregate pod-level requests/limits (the wire format tracks these on
    /// `Pod`, not per-container).
    #[serde(default)]
    pub resources: Option<ResourcesManifest>,
}

#[derive(Debug, Deserialize)]
pub struct ContainerManifest {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub ports: Vec<PortManifest>,
    #[serde(default)]
    pub env: Vec<EnvVarManifest>,
}

#[derive(Debug, Deserialize)]
pub struct PortManifest {
    #[serde(rename = "containerPort")]
    pub container_port: u32,
    #[serde(rename = "hostPort", default)]
    pub host_port: u32,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

#[derive(Debug, Deserialize)]
pub struct EnvVarManifest {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct ResourcesManifest {
    pub requests: Option<ResourceValues>,
    pub limits: Option<ResourceValues>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceValues {
    pub cpu: i32,
    pub memory: i32,
}

impl From<ResourceValues> for Resources {
    fn from(values: ResourceValues) -> Self {
        Resources {
            cpu: values.cpu,
            memory: values.memory,
        }
    }
}

impl PodManifest {
    pub fn from_file(path: &Path) -> Result<Self, CliError> {
        let contents = std::fs::read_to_string(path).map_err(|source| CliError::ReadManifest {
            path: path.to_path_buf(),
            source,
        })?;
        serde_yaml::from_str(&contents).map_err(|source| CliError::ParseManifest {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl TryFrom<PodManifest> for PodWithSpec {
    type Error = CliError;

    fn try_from(manifest: PodManifest) -> Result<Self, CliError> {
        if manifest.kind != "Pod" {
            return Err(CliError::UnsupportedKind(manifest.kind));
        }

        let containers = manifest
            .spec
            .containers
            .into_iter()
            .map(Container::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        let (requests, limits) = match manifest.spec.resources {
            Some(resources) => (
                resources.requests.map(Resources::from),
                resources.limits.map(Resources::from),
            ),
            None => (None, None),
        };

        Ok(PodWithSpec {
            pod: Some(Pod {
                name: manifest.metadata.name,
                status: PodStatus::Pending as i32,
                requests,
                limits,
            }),
            spec: Some(PodSpec {
                namespace: manifest.metadata.namespace,
                containers,
            }),
        })
    }
}

impl TryFrom<ContainerManifest> for Container {
    type Error = CliError;

    fn try_from(manifest: ContainerManifest) -> Result<Self, CliError> {
        let ports = manifest
            .ports
            .into_iter()
            .map(|port| port_from_manifest(port, &manifest.name))
            .collect::<Result<Vec<_>, _>>()?;
        let env = manifest
            .env
            .into_iter()
            .map(|env_var| EnvVar {
                name: env_var.name,
                value: env_var.value,
            })
            .collect();

        Ok(Container {
            name: manifest.name,
            image: manifest.image,
            ports,
            env,
        })
    }
}

fn port_from_manifest(manifest: PortManifest, container_name: &str) -> Result<Port, CliError> {
    let protocol = match manifest.protocol.to_ascii_uppercase().as_str() {
        "TCP" => Protocol::Tcp,
        "UDP" => Protocol::Udp,
        other => {
            return Err(CliError::UnsupportedProtocol {
                container: container_name.to_string(),
                protocol: other.to_string(),
            });
        }
    };

    Ok(Port {
        internal: manifest.container_port,
        external: manifest.host_port,
        protocol: protocol as i32,
    })
}
