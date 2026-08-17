# CNI

Démon réseau du nœud. L'agent l'appelle en gRPC sur `/run/barenetes/cni.sock` pour
attacher un workload au réseau, lire son état, et l'en détacher.

## Lancer

Root obligatoire, `iproute2` et `iptables` installés.

```bash
cargo build --release -p cni
sudo BARENETES_NODE_ID=1 ./target/release/cni
```

Autres variables : `BARENETES_NODE_IP` et `BARENETES_REMOTE_NODE_IPS` pour l'overlay
multi-nœuds, `BARENETES_MTU` (1450 par défaut).

## API

`proto/cni/v1/cni.proto` : `AddWorkloadNetwork`, `GetWorkloadNetwork`,
`DeleteWorkloadNetwork`.

```json
{
  "workload":   { "workload_name": "api", "instance_name": "api-1" },
  "network":    { "network_name": "tenant-a", "vlan_id": 100 },
  "netns_path": "/proc/4242/ns/net"
}
```

Retourne l'IP, la passerelle et l'interface. `Get` et `Delete` prennent le même
`workload` + `network`, sans `netns_path`.

Optionnel dans `Add` : `interface_name` (`eth0` par défaut) et `port_mappings`
(`host_port`, `workload_port`, `protocol`).

## Intégration

1. Démarrer le sandbox du pod, récupérer son PID.
2. `Add` avec `netns_path = /proc/<pid>/ns/net`.
3. `Delete` avant d'arrêter le sandbox.

- `netns_path` doit être exactement `/proc/<pid>/ns/net`, celui de l'hôte est refusé.
- `Add` est idempotent, sauf si les paramètres changent — dans ce cas c'est une erreur.
- `vlan_id` est choisi par l'appelant : le même pour un tenant, jamais partagé entre deux.

## Limite actuelle

Le workload obtient une IP et une route par défaut mais ne joint pas encore sa passerelle
sous filtrage VLAN.
