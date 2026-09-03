# Audit de la CNI Barenetes après migration L3

## Conclusion

La CNI n'utilise plus VXLAN. Le réseau inter-nœuds repose maintenant sur des
routes IPv4 vers le sous-réseau /24 de chaque nœud distant.

Le choix est plus simple pour Barenetes : le bridge, les veth, les VLAN, l'IPAM,
le firewall et le NAT sont conservés, mais l'encapsulation L2 et la FDB ont été
supprimées.

La CNI reste un daemon gRPC interne, et non un plugin CNI Kubernetes standard.

## Changements réalisés

- Suppression du module VXLAN.
- Suppression de l'interface barenetes-vx.
- Suppression du VNI, du port UDP 4789 et de la FDB statique.
- Workloads configurés en /24.
- Gateways VLAN configurées en /24.
- Ajout de routes vers les /24 distants.
- Ajout de BARENETES_REMOTE_NODE_IDS.
- Conservation de BARENETES_REMOTE_NODE_IPS pour les next-hop underlay.
- MTU par défaut ramené à 1500.
- Tests d'intégration adaptés au routage L3.
- Déploiement et documentation adaptés.

## Fonctionnement du routage

Pour un VLAN 100 :

~~~text
nœud 1 : 10.100.1.0/24, gateway 10.100.1.1
nœud 2 : 10.100.2.0/24, gateway 10.100.2.1
~~~

Sur le nœud 1, la CNI installe :

~~~text
10.100.2.0/24 via <IP-underlay-du-nœud-2>
~~~

Sur le nœud 2, elle installe la route inverse.

Les IPs et les IDs distants sont associés par position :

~~~text
BARENETES_REMOTE_NODE_IPS=192.168.1.12,192.168.1.13
BARENETES_REMOTE_NODE_IDS=2,3
~~~

Les deux listes doivent avoir la même longueur. Les IDs distants doivent être
uniques et compris entre 1 et 255.

## Limites restantes

### Routes statiques

Les routes sont installées lorsqu'une interface VLAN est créée. Il n'y a pas de
découverte dynamique ni de protocole de routage.

Ajouter un nœud nécessite donc de modifier la configuration des nœuds existants
et de redémarrer leur daemon. Cette approche est adaptée à un petit cluster
fixe.

### Configuration de l'underlay

La CNI ne configure pas l'interface physique ni les routes permettant de joindre
les IPs underlay. L'administrateur doit fournir :

- des IPs underlay joignables ;
- une connectivité IP entre les nœuds ;
- un chemin retour ;
- un firewall qui autorise ce trafic.

Une route comme 10.100.2.0/24 via 192.168.1.2 suppose que le next-hop est
joignable par la table de routage de l'underlay.

### Unicité des node_id

Le node_id est encodé dans le troisième octet IPv4. Deux nœuds ne doivent
jamais partager le même ID.

La CNI valide la forme de l'ID, mais ne peut pas garantir son unicité dans le
cluster.

### VLANs

Les VLANs restent limités à 1-255, car le VLAN est encodé dans le deuxième octet
de l'adresse 10.<vlan>.<node>.<workload>.

### Firewall

Le firewall reste géré par iptables. Il :

- active le forwarding IPv4 ;
- autorise le trafic nécessaire des tenants ;
- bloque le trafic routé entre VLANs ;
- effectue le MASQUERADE vers l'extérieur ;
- gère encore les mappings de ports.

La migration L3 ne supprime donc pas la dépendance à iptables, br_netfilter ou
aux privilèges root.

### Port forwarding

Le DNAT/SNAT et le réseau de base sont toujours dans le même daemon. Le port
forwarding reste une responsabilité indépendante qui pourrait être extraite
plus tard pour réduire la surface de la CNI.

### API

L'API reste :

~~~text
AddWorkloadNetwork
GetWorkloadNetwork
DeleteWorkloadNetwork
~~~

Elle est exposée par gRPC sur un socket Unix. Elle n'implémente pas les
opérations stdin/stdout ADD, DEL, CHECK, VERSION d'un plugin CNI standard.

### Réconciliation

La réconciliation est exécutée au démarrage. Elle reconstruit les états locaux,
les pools IP, les interfaces VLAN et les mappings de ports. Elle ne redistribue
pas les routes et ne surveille pas continuellement les changements de topologie.

## Pourquoi cette version est plus simple

La version L3 ne nécessite plus :

- d'interface VXLAN ;
- de VNI ;
- de port UDP 4789 ;
- de FDB ;
- de transport broadcast/ARP entre les nœuds ;
- de MTU réduit pour l'encapsulation.

Le datapath inter-nœuds est maintenant observable avec les outils Linux habituels :

~~~text
ip route
ip addr
ip neigh
ping
tcpdump
~~~

## Validation nécessaire

La validation source attendue est :

~~~sh
cargo test -p cni --locked
cargo clippy -p cni --all-targets --locked -- -D warnings
cargo build -p cni --locked
sudo ./cni/tests/integration.sh
~~~

Les tests unitaires et la compilation ne suffisent pas à prouver le routage
Linux réel. Le test d'intégration privilégié doit confirmer :

- la création des routes ;
- la communication entre workloads de nœuds différents ;
- l'isolation entre VLANs ;
- le NAT sortant ;
- la reprise après redémarrage ;
- le nettoyage des états.

## Verdict

La migration L3 est adaptée à un petit cluster Barenetes. Elle réduit la
complexité réseau et supprime la dépendance à VXLAN.

La prochaine difficulté éventuelle sera la distribution dynamique des routes.
Pour deux ou quelques nœuds, la configuration statique reste suffisante.
