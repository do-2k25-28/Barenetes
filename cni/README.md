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

Le test d'intégration crée temporairement un réseau Linux isolé afin de ne pas
modifier le réseau principal de la machine. Il exécute les étapes suivantes :

1. Démarrage du daemon CNI et création du socket Unix.
2. Vérification du bridge `barenetes0` et de l'interface VXLAN.
3. Connexion de deux workloads au même VLAN avec `AddWorkloadNetwork`.
4. Connexion d'un troisième workload à un autre VLAN.
5. Vérification que les workloads du même VLAN communiquent et que le trafic
   inter-VLAN est bloqué, avec contrôle de l'adresse IPv4 de la gateway, de
   l'adresse du workload et de sa route par défaut.
6. Vérification de `GetWorkloadNetwork`, de l'ADD idempotent et de
   `DeleteWorkloadNetwork`.
7. Vérification que les fichiers d'état des workloads sont supprimés.

Le script affiche `OK - intégration CNI complète réussie` uniquement si toutes
les étapes réussissent. Il nettoie automatiquement les namespaces, le daemon,
le socket et les fichiers d'état temporaires, même en cas d'échec.
