# Fonctionnement de la CNI Barenetes

## Vue générale

Un daemon CNI est lancé sur chaque nœud worker. L'agent lui parle par gRPC sur
un socket Unix.

~~~text
agent
  |
  | gRPC sur /run/barenetes/cni.sock
  v
CNI daemon
  |
  +-- bridge Linux barenetes0
  +-- veth vers les namespaces workloads
  +-- VLAN et gateways
  +-- routes IPv4 vers les nœuds distants
  +-- IPAM et état persistant
  +-- iptables : forwarding, NAT et DNAT
~~~

L'API est définie dans proto/cni/v1/cni.proto. Elle est interne à Barenetes et
n'est pas l'interface stdin/stdout d'un plugin CNI Kubernetes standard.

## Composants

### Client de l'agent

Le client est dans agent/src/cni.rs. Il :

- ouvre une connexion vers le socket Unix ;
- envoie la référence du workload et du réseau ;
- transmet le PID sous la forme /proc/<pid>/ns/net ;
- demande l'ajout ou la suppression du réseau ;
- récupère l'IP et la gateway retournées.

Il se reconnecte pour chaque opération, ce qui permet de supporter un
redémarrage du daemon CNI.

### Serveur gRPC

Le serveur est créé dans cni/src/runtime.rs et les méthodes sont dans
cni/src/runtime/handler.rs.

Les méthodes sont :

~~~text
AddWorkloadNetwork
GetWorkloadNetwork
DeleteWorkloadNetwork
~~~

Les appels Linux sont bloquants. Le serveur les exécute dans
spawn_blocking. Un mutex sérialise les opérations d'un même daemon afin
d'éviter des modifications concurrentes du bridge, de l'IPAM et de l'état.

### Socket Unix

cni/src/runtime/socket.rs :

1. crée le répertoire parent ;
2. supprime un ancien socket ;
3. refuse d'écraser un fichier qui n'est pas un socket ;
4. crée le socket en 0660.

Le déploiement doit donner à l'agent les permissions d'accès.

### Bridge Linux

cni/src/network/bridge.rs crée barenetes0.

Le bridge est :

- créé s'il n'existe pas ;
- configuré avec le MTU choisi ;
- activé ;
- configuré avec vlan_filtering=1.

Il relie les ports veth et les interfaces VLAN. En multi-nœud, le trafic sort
par des routes IPv4 vers les sous-réseaux distants. Le bridge ne porte pas
directement la gateway IP.

### Veth

Pour chaque workload, cni/src/network/workload.rs crée une paire :

~~~text
namespace du nœud              namespace du workload

v<hash>  <------------------>  peer puis eth0
  |
  +-- port de barenetes0
~~~

Le peer est déplacé dans le namespace du PID donné par l'agent, renommé selon
interface_name, configuré avec l'IP, puis activé.

Le côté nœud est attaché au bridge et configuré comme port non taggé du VLAN du
tenant.

### VLAN et gateway

cni/src/network/vlan.rs crée une sous-interface du bridge, par exemple
barenetes0.100.

Pour un VLAN vlan et un nœud node, la gateway est :

~~~text
10.<vlan>.<node>.1
~~~

Exemples :

~~~text
VLAN 100, nœud 1 : 10.100.1.1
VLAN 100, nœud 2 : 10.100.2.1
~~~

Le veth du workload est placé dans le VLAN correspondant. La sous-interface
VLAN porte la gateway IP et permet au workload d'atteindre le réseau externe.

### Adressage

cni/src/addressing.rs réserve :

~~~text
10.<vlan>.<node>.2 à 10.<vlan>.<node>.254
~~~

L'adresse .1 est la gateway.

Exemple :

~~~text
gateway : 10.100.1.1
pool    : 10.100.1.2 - 10.100.1.254
~~~

Dans l'implémentation actuelle, les workloads utilisent /24. Chaque nœud
possède donc un sous-réseau routable différent pour un même tenant.

### IPAM

cni/src/ip_pool.rs conserve un pool JSON par VLAN :

~~~text
ip-pool.json
ip-pool.lock
~~~

Allocation :

1. créer le répertoire ;
2. verrouiller le fichier ;
3. lire l'état ;
4. choisir la première IP libre ;
5. réécrire l'état atomiquement ;
6. libérer le verrou.

L'IPAM est persistant et protégé contre la concurrence sur un même nœud. Il
n'est pas distribué. L'absence de collision entre nœuds dépend de node_id
uniques.

### Routage inter-nœuds

Le routage est configuré lorsqu'un workload utilise un VLAN et que
BARENETES_REMOTE_NODE_IPS n'est pas vide. Chaque IP distante est associée à
un identifiant dans BARENETES_REMOTE_NODE_IDS, dans le même ordre.

Pour le VLAN 100 et le nœud distant 2, la CNI installe :

~~~text
10.100.2.0/24 via <IP-underlay-du-nœud-2>
~~~

Le chemin d'un paquet est :

~~~text
workload -> gateway locale -> route IPv4 -> underlay -> gateway distante
~~~

L'underlay doit déjà fournir des IPs de nœuds joignables et des routes entre
ces IPs. La CNI ne configure pas l'interface physique underlay.

### Forwarding et firewall

cni/src/network/firewall.rs active :

~~~text
net.ipv4.ip_forward=1
~~~

Puis crée les chaînes :

~~~text
BARENETES-FORWARD
BARENETES-PREROUTING
BARENETES-OUTPUT
~~~

Les règles :

- acceptent ESTABLISHED,RELATED ;
- autorisent le trafic entrant et sortant des interfaces VLAN ;
- autorisent le chemin bridge nécessaire ;
- bloquent le trafic routé entre interfaces VLAN ;
- font du MASQUERADE vers l'extérieur du bridge tenant.

Le but est :

~~~text
workload -> gateway -> réseau externe
~~~

tout en empêchant le routage direct entre tenants.

### Port forwarding

Les mappings d'ADD sont validés :

- ports entre 1 et 65535 ;
- protocoles TCP ou UDP ;
- pas de doublon de port externe pour un protocole.

La CNI installe ensuite des règles DNAT et les règles de forwarding associées.

Exemple :

~~~text
host:8080 -> workload 10.100.1.2:80
~~~

Les mappings sont conservés dans l'état afin d'être réinstallés après un
redémarrage ou une suppression des règles iptables.

### État persistant

cni/src/state.rs écrit un fichier JSON par connexion réseau.

Il contient notamment :

- workload et instance ;
- réseau ;
- veth côté nœud ;
- interface dans le workload ;
- IP ;
- gateway ;
- VLAN ;
- mappings de ports.

Les écritures sont atomiques. L'état permet de rendre ADD idempotent et de
reconstruire une partie du réseau après redémarrage.

## Cycle de vie

### ADD

Le chemin est :

1. l'agent envoie AddWorkloadNetwork ;
2. la CNI valide les noms, références, PID, namespace et VLAN ;
3. elle vérifie si une connexion identique existe ;
4. elle lit le node_id et installe les routes du VLAN ;
5. elle crée ou vérifie la sous-interface VLAN et la gateway ;
6. elle réserve une IP ;
7. elle crée la paire veth ;
8. elle déplace le peer dans le namespace du workload ;
9. elle renomme et configure l'interface ;
10. elle ajoute la route par défaut ;
11. elle attache le veth au bridge et au VLAN ;
12. elle installe les mappings de ports ;
13. elle écrit l'état ;
14. elle renvoie l'IP et la gateway.

En cas d'erreur, la CNI tente de supprimer le veth et de libérer l'IP.

### GET

La CNI :

1. lit l'état JSON ;
2. vérifie que le veth existe ;
3. renvoie READY si le veth existe ;
4. renvoie ERROR si l'état existe mais que le veth a disparu.

GET ne recrée pas le réseau.

### DEL

La CNI :

1. charge l'état ;
2. supprime le veth s'il existe ;
3. supprime les mappings ;
4. libère l'IP ;
5. supprime l'état.

Si l'état n'existe plus, DEL réussit quand même. L'opération est donc
idempotente. Si la libération IPAM échoue, l'état est conservé pour permettre
une nouvelle tentative.

## Démarrage

cni/src/runtime.rs effectue :

1. création de l'IPAM ;
2. création du bridge ;
3. préparation du routage à la demande ;
4. activation du forwarding et installation du firewall ;
5. réconciliation ;
6. ouverture du socket gRPC.

## Réconciliation

cni/src/network/reconcile.rs compare l'état persistant avec les interfaces
Linux présentes.

Elle peut :

- supprimer les états dont le veth a disparu ;
- reconstruire les pools IP ;
- supprimer certains veth orphelins ;
- recréer les interfaces VLAN ;
- réinstaller les mappings de ports.

Elle s'exécute au démarrage. Ce n'est pas une boucle de contrôle permanente.

## Exemple complet

Pour un workload api-1 du tenant tenant-a, VLAN 100, nœud 1 :

~~~text
IP workload       10.100.1.2/24
Gateway           10.100.1.1
Interface         eth0
VLAN              100
~~~

Chemin local :

~~~text
api-1:eth0
  -> veth
  -> barenetes0 VLAN 100
  -> barenetes0.100
  -> 10.100.1.1
~~~

Chemin vers un workload du nœud 2 :

~~~text
api-1
  -> barenetes0
  -> gateway 10.100.1.1
  -> route 10.100.2.0/24 via l'underlay du nœud 2
  -> gateway 10.100.2.1
  -> veth distant
~~~

## Limites essentielles

- Routes statiques et configuration manuelle des nœuds.
- Domaine L2 limité à chaque nœud ; le trafic inter-nœuds est routé.
- IPAM local et non distribué.
- Dépendance à Linux, root et plusieurs commandes système.
- API gRPC interne, pas CNI standard.
- MTU et underlay non gérés par la CNI.
- Réconciliation uniquement au démarrage.
- Firewall et port forwarding mélangés au réseau de base.
