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
{"port_mappings": [{"external": 8080, "internal": 80, "protocol": "TCP"}]}
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

Le test d'intégration crée temporairement un réseau Linux isolé afin de ne pas
modifier le réseau principal de la machine. Il force aussi
`bridge-nf-call-iptables=1` et la politique `FORWARD=DROP` pour tester le
chemin du bridge avec `br_netfilter`. Il exécute quatre parcours :

1. Un nœud avec un workload.
2. Un nœud avec plusieurs workloads.
3. Plusieurs nœuds avec un workload par nœud, reliés par VXLAN.
4. Plusieurs nœuds avec plusieurs workloads par nœud, reliés par VXLAN.

Chaque parcours vérifie la connexion réseau, l'adressage IPv4 et le nettoyage
des états. Les scénarios multi-nœuds lancent deux daemons CNI dans deux
namespaces réseau séparés et utilisent un bridge de transport temporaire.

Le script affiche `OK - les quatre parcours CNI ont réussi` uniquement si toutes
les étapes réussissent. Il nettoie automatiquement les namespaces, les
daemons, le bridge de transport, les sockets et les fichiers d'état temporaires,
même en cas d'échec.
