# CNI

Démon réseau local appelé par l’agent via gRPC pour connecter les workloads au réseau du nœud.

## Fonctionnalités

- Bridge Linux, veth et allocation persistante d’adresses IP.
- Isolation VLAN et overlay VXLAN multi-nœuds.
- Règles firewall et redirection de ports TCP/UDP.
- API gRPC `AddWorkloadNetwork`, `GetWorkloadNetwork` et `DeleteWorkloadNetwork`.

## Exemple

```bash
cargo build --release -p cni
sudo BARENETES_NODE_ID=1 ./target/release/cni
```

L’agent appelle ensuite le daemon sur `/run/barenetes/cni.sock` avec le contrat
`proto/cni/v1/cni.proto`.
