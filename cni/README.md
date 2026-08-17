# CNI

Démon réseau local appelé par l’agent via gRPC pour connecter les workloads au réseau du nœud.

## Fonctionnalités

- Bridge Linux, veth et allocation persistante d’adresses IP.
- Isolation VLAN et overlay VXLAN multi-nœuds.
- Règles firewall et redirection de ports TCP/UDP.
- API gRPC `AddWorkloadNetwork`, `GetWorkloadNetwork` et `DeleteWorkloadNetwork`.

## Exemples

```bash
cargo build --release -p cni
sudo BARENETES_NODE_ID=1 ./target/release/cni
```

Le daemon configure le bridge, l’IPAM et le firewall sur le nœud.

Connexion d’un workload avec veth, IP et VLAN :

```json
{
  "workload": {"workload_name": "api", "instance_name": "api-1"},
  "network": {"network_name": "tenant-a", "vlan_id": 100},
  "netns_path": "/proc/4242/ns/net",
  "interface_name": "eth0"
}
```

Overlay VXLAN multi-nœuds :

```bash
BARENETES_NODE_ID=1 \
BARENETES_NODE_IP=192.168.1.10 \
BARENETES_REMOTE_NODE_IPS=192.168.1.11 \
./target/release/cni
```

Redirection firewall TCP/UDP dans `AddWorkloadNetwork` :

```json
{"port_mappings": [{"host_port": 8080, "workload_port": 80, "protocol": "PORT_PROTOCOL_TCP"}]}
```

L’agent utilise ensuite `GetWorkloadNetwork` puis `DeleteWorkloadNetwork` sur
`/run/barenetes/cni.sock`, selon le contrat `proto/cni/v1/cni.proto`.
