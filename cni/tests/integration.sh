#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CNI_BIN="$ROOT/target/debug/cni"
CLIENT_BIN="$ROOT/target/debug/cni-integration-client"
TMP_DIR=
TRANSPORT_BRIDGE=cni-xport
NETNS_PIDS=()
DAEMON_PIDS=()
NODE_HOST_LINKS=()
NODE_SOCKETS=()
NODE_STATES=()
NODE_NETNS_PIDS=()
LAST_NETNS_PID=

fail() { echo "FAIL: $*" >&2; exit 1; }

cleanup() {
    set +e
    for pid in "${DAEMON_PIDS[@]}"; do kill "$pid" 2>/dev/null; done
    for pid in "${DAEMON_PIDS[@]}"; do wait "$pid" 2>/dev/null; done
    for pid in "${NETNS_PIDS[@]}"; do kill "$pid" 2>/dev/null; done
    for pid in "${NETNS_PIDS[@]}"; do wait "$pid" 2>/dev/null; done
    for link in "${NODE_HOST_LINKS[@]}"; do ip link delete "$link" 2>/dev/null; done
    ip link delete "$TRANSPORT_BRIDGE" type bridge 2>/dev/null
    [[ -n "$TMP_DIR" ]] && rm -rf "$TMP_DIR"
}

verify_cleanup() {
    local clean=1
    for pid in "${DAEMON_PIDS[@]}" "${NETNS_PIDS[@]}"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "FAIL: processus encore actif après nettoyage: $pid" >&2
            clean=0
        fi
    done
    [[ "$clean" == 1 ]]
}

on_exit() {
    local status=$?
    trap - EXIT
    cleanup
    if verify_cleanup; then echo "[post-nettoyage] OK"; else status=1; fi
    exit "$status"
}
trap on_exit EXIT

[[ "$(id -u)" == 0 ]] || fail "ce test doit être exécuté en root"
for command in ip bridge iptables nsenter timeout unshare ping modprobe sysctl; do
    command -v "$command" >/dev/null || fail "commande manquante: $command"
done
[[ -x "$CNI_BIN" ]] || fail "binaire CNI absent; lancer: cargo build -p cni"
[[ -x "$CLIENT_BIN" ]] || fail "client absent; lancer: cargo build -p cni"

if [[ -z "${BARENETES_INTEGRATION_NETNS:-}" ]]; then
    exec env BARENETES_INTEGRATION_NETNS=1 unshare -n -- "$0" "$@"
fi

TMP_DIR=$(mktemp -d /tmp/barenetes-cni-integration.XXXXXX)
modprobe br_netfilter || fail "le module br_netfilter est indisponible"
sysctl -q -w net.bridge.bridge-nf-call-iptables=1 \
    || fail "bridge-nf-call-iptables ne peut pas être activé"
ip link add "$TRANSPORT_BRIDGE" type bridge
ip link set "$TRANSPORT_BRIDGE" up

start_netns() {
    unshare -n sleep 600 &
    LAST_NETNS_PID=$!
    NETNS_PIDS+=("$LAST_NETNS_PID")
}

start_node() {
    local node_id=$1 node_ip=$2 remote_ip=$3
    local pid socket state uplink host_uplink log
    start_netns
    pid=$LAST_NETNS_PID
    socket="$TMP_DIR/cni-node-${node_id}.sock"
    state="$TMP_DIR/state-node-${node_id}/workloads"
    uplink="cni-up-${node_id}"
    host_uplink="cni-host-${node_id}"
    log="$TMP_DIR/cni-node-${node_id}.log"

    ip link add "$host_uplink" type veth peer name "$uplink"
    NODE_HOST_LINKS+=("$host_uplink")
    ip link set "$uplink" netns "$pid"
    ip link set "$host_uplink" master "$TRANSPORT_BRIDGE"
    ip link set "$host_uplink" up
    nsenter -t "$pid" -n ip link set lo up
    nsenter -t "$pid" -n ip addr add "$node_ip/24" dev "$uplink"
    nsenter -t "$pid" -n ip link set "$uplink" up

    nsenter -t "$pid" -n env \
        BARENETES_NODE_ID="$node_id" \
        BARENETES_REMOTE_NODE_IPS="$remote_ip" \
        BARENETES_REMOTE_NODE_IDS="$([[ "$node_id" == 1 ]] && echo 2 || echo 1)" \
        BARENETES_CNI_SOCKET="$socket" \
        BARENETES_CNI_STATE_DIR="$state" \
        BARENETES_CNI_IP_POOL_DIR="$TMP_DIR/pool-node-${node_id}" \
        "$CNI_BIN" >"$log" 2>&1 &
    DAEMON_PIDS+=("$!")
    NODE_SOCKETS[$node_id]="$socket"
    NODE_STATES[$node_id]="$state"
    NODE_NETNS_PIDS[$node_id]="$pid"
    for _ in $(seq 1 50); do
        [[ -S "$socket" ]] && break
        kill -0 "${DAEMON_PIDS[-1]}" 2>/dev/null || { cat "$log" >&2; fail "daemon du nœud $node_id arrêté"; }
        sleep 0.1
    done
    [[ -S "$socket" ]] || { cat "$log" >&2; fail "socket du nœud $node_id absente"; }
    nsenter -t "$pid" -n iptables -P FORWARD DROP
}

# Redémarre uniquement le daemon d'un nœud déjà en place (son netns, son
# bridge, ses veth restent vivants) : c'est le scénario "crash sur un système
# vivant" que reconcile() doit couvrir, distinct d'un reboot complet déjà
# couvert par ailleurs. daemon_index doit pointer sur l'entrée de DAEMON_PIDS
# créée par le start_node correspondant.
restart_node_daemon() {
    local node_id=$1 node_ip=$2 remote_ip=$3 daemon_index=$4
    local pid=${NODE_NETNS_PIDS[$node_id]}
    local socket=${NODE_SOCKETS[$node_id]}
    local state=${NODE_STATES[$node_id]}
    local log="$TMP_DIR/cni-node-${node_id}-restart.log"

    kill -9 "${DAEMON_PIDS[$daemon_index]}" 2>/dev/null
    wait "${DAEMON_PIDS[$daemon_index]}" 2>/dev/null
    rm -f "$socket"

    nsenter -t "$pid" -n env \
        BARENETES_NODE_ID="$node_id" \
        BARENETES_REMOTE_NODE_IPS="$remote_ip" \
        BARENETES_REMOTE_NODE_IDS="$([[ "$node_id" == 1 ]] && echo 2 || echo 1)" \
        BARENETES_CNI_SOCKET="$socket" \
        BARENETES_CNI_STATE_DIR="$state" \
        BARENETES_CNI_IP_POOL_DIR="$TMP_DIR/pool-node-${node_id}" \
        "$CNI_BIN" >"$log" 2>&1 &
    DAEMON_PIDS[$daemon_index]="$!"
    for _ in $(seq 1 50); do
        [[ -S "$socket" ]] && return 0
        kill -0 "${DAEMON_PIDS[$daemon_index]}" 2>/dev/null \
            || { cat "$log" >&2; fail "daemon du nœud $node_id n'a pas redémarré"; }
        sleep 0.1
    done
    cat "$log" >&2
    fail "socket du nœud $node_id absente après redémarrage"
}

start_workload() {
    start_netns
}

add_workload() {
    local node_id=$1 instance=$2 network=$3 vlan=$4 pid=$5 port_mapping=${6:-}
    BARENETES_CNI_SOCKET="${NODE_SOCKETS[$node_id]}" "$CLIENT_BIN" \
        add "$instance" "$network" "$vlan" "/proc/$pid/ns/net" $port_mapping
}

delete_workload() {
    local node_id=$1 instance=$2 network=$3 vlan=$4
    BARENETES_CNI_SOCKET="${NODE_SOCKETS[$node_id]}" "$CLIENT_BIN" \
        delete "$instance" "$network" "$vlan"
}

assert_ping() {
    local pid=$1 address=$2 message=$3
    nsenter -t "$pid" -n ping -c 1 -W 1 "$address" >/dev/null || fail "$message"
}

echo "[1/5] un nœud avec un workload"
start_node 1 192.0.2.1 192.0.2.2
start_workload; W1=$LAST_NETNS_PID
add_workload 1 single-a tenant-single 100 "$W1"
nsenter -t "$W1" -n ip -4 addr show dev eth0 | grep -q '10.100.1.2/24' || fail "IP du workload unique absente"
assert_ping "$W1" 10.100.1.1 "gateway inaccessible"
delete_workload 1 single-a tenant-single 100

echo "[2/5] un nœud avec plusieurs workloads"
start_workload; W2=$LAST_NETNS_PID
start_workload; W3=$LAST_NETNS_PID
add_workload 1 single-a tenant-many 101 "$W2"
add_workload 1 single-b tenant-many 101 "$W3"
assert_ping "$W2" 10.101.1.3 "communication locale A → B échouée"
assert_ping "$W3" 10.101.1.2 "communication locale B → A échouée"
delete_workload 1 single-a tenant-many 101
delete_workload 1 single-b tenant-many 101

echo "[3/5] plusieurs nœuds avec un workload par nœud"
start_node 2 192.0.2.2 192.0.2.1
start_workload; W4=$LAST_NETNS_PID
start_workload; W5=$LAST_NETNS_PID
add_workload 1 multi-a tenant-multi-one 102 "$W4"
add_workload 2 multi-b tenant-multi-one 102 "$W5"
nsenter -t "${NODE_NETNS_PIDS[1]}" -n ip route show 10.102.2.0/24 \
    | grep -q 'via 192.0.2.2' || fail "route vers le nœud 2 absente"
nsenter -t "${NODE_NETNS_PIDS[2]}" -n ip route show 10.102.1.0/24 \
    | grep -q 'via 192.0.2.1' || fail "route vers le nœud 1 absente"
assert_ping "$W4" 10.102.2.2 "routage nœud 1 → nœud 2 échoué"
assert_ping "$W5" 10.102.1.2 "routage nœud 2 → nœud 1 échoué"
delete_workload 1 multi-a tenant-multi-one 102
delete_workload 2 multi-b tenant-multi-one 102

echo "[4/5] plusieurs nœuds avec plusieurs workloads par nœud"
start_workload; W6=$LAST_NETNS_PID
start_workload; W7=$LAST_NETNS_PID
start_workload; W8=$LAST_NETNS_PID
start_workload; W9=$LAST_NETNS_PID
add_workload 1 multi-a-1 tenant-multi-many 103 "$W6"
add_workload 1 multi-a-2 tenant-multi-many 103 "$W7"
add_workload 2 multi-b-1 tenant-multi-many 103 "$W8"
add_workload 2 multi-b-2 tenant-multi-many 103 "$W9"
nsenter -t "${NODE_NETNS_PIDS[1]}" -n ip route show 10.103.2.0/24 \
    | grep -q 'via 192.0.2.2' || fail "route multi-workloads vers le nœud 2 absente"
assert_ping "$W6" 10.103.1.3 "communication locale nœud 1 échouée"
assert_ping "$W6" 10.103.2.2 "communication routée nœud 1 → nœud 2 échouée"
assert_ping "$W7" 10.103.2.3 "communication routée nœud 1 → nœud 2 échouée"
assert_ping "$W8" 10.103.1.2 "communication routée nœud 2 → nœud 1 échouée"
delete_workload 1 multi-a-1 tenant-multi-many 103
delete_workload 1 multi-a-2 tenant-multi-many 103
delete_workload 2 multi-b-1 tenant-multi-many 103
delete_workload 2 multi-b-2 tenant-multi-many 103

echo "[5/5] réconciliation au redémarrage : veth orpheline et iptables -F sur un nœud vivant"
N1=${NODE_NETNS_PIDS[1]}
start_workload; W10=$LAST_NETNS_PID
add_workload 1 reconcile-a tenant-reconcile 108 "$W10" 8080:80
nsenter -t "$N1" -n iptables -t nat -S | grep -q '10.108.1.2:80' \
    || fail "règle DNAT absente juste après l'ajout du workload"
nsenter -t "$N1" -n ip link add vdeadbeef00 type veth peer name pdeadbeef00
nsenter -t "$N1" -n ip link set vdeadbeef00 master barenetes0
nsenter -t "$N1" -n ip link set vdeadbeef00 up
nsenter -t "$N1" -n iptables -F
nsenter -t "$N1" -n iptables -t nat -F
restart_node_daemon 1 192.0.2.1 192.0.2.2 0
nsenter -t "$N1" -n ip link show vdeadbeef00 >/dev/null 2>&1 \
    && fail "le veth orphelin vdeadbeef00 est toujours là après redémarrage"
nsenter -t "$N1" -n iptables -t nat -S | grep -q '10.108.1.2:80' \
    || fail "la règle DNAT n'a pas été réinstallée après iptables -F"
nsenter -t "$N1" -n iptables -t nat -S POSTROUTING | grep -q MASQUERADE \
    || fail "le MASQUERADE du tenant n'a pas été réinstallé après iptables -F"
assert_ping "$W10" 10.108.1.1 "passerelle du tenant injoignable après reconciliation"

delete_workload 1 reconcile-a tenant-reconcile 108

for state in "${NODE_STATES[@]}"; do
    if [[ -d "$state" ]] && find "$state" -type f -name '*.json' -print -quit | grep -q .; then
        fail "des fichiers d'état restent dans $state"
    fi
done
echo "OK - les cinq parcours CNI ont réussi"
