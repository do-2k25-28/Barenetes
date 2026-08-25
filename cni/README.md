# CNI

Local network daemon called by the agent over gRPC to connect workloads to the node network.

## Features

- Linux bridge, veth interfaces, and persistent IP address allocation.
- VLAN isolation and multi-node VXLAN overlay.
- Firewall rules and TCP/UDP port forwarding.
- gRPC API: `AddWorkloadNetwork`, `GetWorkloadNetwork`, and `DeleteWorkloadNetwork`.

## Examples

```bash
cargo build --release -p cni
sudo BARENETES_NODE_ID=1 ./target/release/cni
```

The daemon configures the bridge, IPAM, and firewall on the node.

Connect a workload with veth, IP, and VLAN:

```json
{
  "workload": {"workload_name": "api", "instance_name": "api-1"},
  "network": {"network_name": "tenant-a", "vlan_id": 100},
  "netns_path": "/proc/4242/ns/net",
  "interface_name": "eth0"
}
```

Multi-node VXLAN overlay:

```bash
BARENETES_NODE_ID=1 \
BARENETES_NODE_IP=192.168.1.10 \
BARENETES_REMOTE_NODE_IPS=192.168.1.11 \
./target/release/cni
```

TCP/UDP firewall forwarding in `AddWorkloadNetwork`:

```json
{"port_mappings": [{"host_port": 8080, "workload_port": 80, "protocol": "PORT_PROTOCOL_TCP"}]}
```

The agent then uses `GetWorkloadNetwork` and `DeleteWorkloadNetwork` through
`/run/barenetes/cni.sock`, according to the `proto/cni/v1/cni.proto` contract.

## Tests

Depuis la racine du dépôt :

```bash
# Tests unitaires de la CNI
cargo test -p cni

# Vérification Clippy
cargo clippy -p cni --all-targets -- -D warnings

# Test d'intégration réseau complet (root requis)
cargo build -p cni
sudo ./cni/tests/integration.sh
```

Le test d'intégration crée temporairement des namespaces réseau, un bridge,
des VLANs et un VXLAN. Il vérifie la connectivité dans un même VLAN,
l'isolation entre VLANs, l'idempotence de l'API et le nettoyage de l'état.
