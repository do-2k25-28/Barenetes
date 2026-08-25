#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CNI_BIN="$ROOT/target/debug/cni"
SOCKET=/run/barenetes/cni.sock
STATE_DIR=/var/lib/barenetes/cni
TEST_NETNS=${BARENETES_INTEGRATION_NETNS:-}
DAEMON_PID=
NS_A=
NS_B=
NS_C=
TMP_DIR=
STATE_OWNED=0

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

cleanup() {
    set +e
    [[ -n "$DAEMON_PID" ]] && kill "$DAEMON_PID" 2>/dev/null
    [[ -n "$DAEMON_PID" ]] && wait "$DAEMON_PID" 2>/dev/null
    for pid in "$NS_A" "$NS_B" "$NS_C"; do
        [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
    done
    [[ -n "$NS_A" ]] && wait "$NS_A" 2>/dev/null
    [[ -n "$NS_B" ]] && wait "$NS_B" 2>/dev/null
    [[ -n "$NS_C" ]] && wait "$NS_C" 2>/dev/null
    rm -f "$SOCKET"
    [[ "$STATE_OWNED" == 1 ]] && rm -rf "$STATE_DIR"
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

[[ "$(id -u)" == 0 ]] || fail "ce test doit être exécuté en root"

for command in ip bridge iptables nsenter timeout unshare; do
    command -v "$command" >/dev/null || fail "commande manquante: $command"
done

if [[ -z "$TEST_NETNS" ]]; then
    exec env BARENETES_INTEGRATION_NETNS=1 unshare -n -- "$0" "$@"
fi

[[ ! -e "$SOCKET" ]] || fail "$SOCKET existe déjà; arrêt pour ne pas toucher à un daemon actif"
[[ ! -e "$STATE_DIR" ]] || fail "$STATE_DIR existe déjà; supprimer ou sauvegarder cet état avant le test"
STATE_OWNED=1

[[ -x "$CNI_BIN" ]] || fail "binaire CNI absent; lancer d'abord: cargo build -p cni"
[[ -x "$ROOT/target/debug/cni-integration-client" ]] || fail "client de test absent; lancer d'abord: cargo build -p cni"

TMP_DIR=$(mktemp -d /tmp/barenetes-cni-integration.XXXXXX)

echo "[1/7] démarrage du daemon CNI"
BARENETES_NODE_ID=1 \
BARENETES_NODE_IP=127.0.0.1 \
BARENETES_REMOTE_NODE_IPS=127.0.0.2 \
    "$CNI_BIN" >"$TMP_DIR/cni.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 50); do
    [[ -S "$SOCKET" ]] && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { cat "$TMP_DIR/cni.log" >&2; fail "le daemon CNI s'est arrêté"; }
    sleep 0.1
done
[[ -S "$SOCKET" ]] || { cat "$TMP_DIR/cni.log" >&2; fail "socket CNI non créée"; }

echo "[2/7] vérification bridge, VXLAN et firewall"
timeout 5 ip link show dev barenetes0 >/dev/null \
    || fail "bridge barenetes0 absent"
timeout 5 ip link show dev barenetes-vx >/dev/null \
    || fail "interface VXLAN barenetes-vx absente"
FDB=$(timeout 5 bridge fdb show dev barenetes-vx dst 127.0.0.2) \
    || fail "lecture de la table FDB VXLAN impossible"
grep -q 127.0.0.2 <<<"$FDB" \
    || fail "destination VXLAN 127.0.0.2 absente de la table FDB"
timeout 5 iptables -t filter -C BARENETES-FORWARD \
    -i barenetes0+ -o barenetes0+ -j DROP \
    || fail "règle d'isolation inter-VLAN absente d'iptables"

start_netns() {
    unshare -n sleep 600 &
    echo "$!"
}

NS_A=$(start_netns)
NS_B=$(start_netns)
NS_C=$(start_netns)

grpc() {
    "$ROOT/target/debug/cni-integration-client" "$@"
}

echo "[3/7] ADD de deux workloads dans le même VLAN"
grpc add a tenant-a 100 "/proc/$NS_A/ns/net"
grpc add b tenant-a 100 "/proc/$NS_B/ns/net"

echo "[4/7] ADD d'un workload dans un autre VLAN"
grpc add c tenant-b 200 "/proc/$NS_C/ns/net"

echo "[5/7] connectivité même VLAN et isolation inter-VLAN"
nsenter -t "$NS_A" -n ping -c 1 -W 1 10.100.1.3 >/dev/null
if nsenter -t "$NS_A" -n ping -c 1 -W 1 10.200.1.2 >/dev/null 2>&1; then
    fail "le trafic inter-VLAN est encore autorisé"
fi

echo "[6/7] idempotence GET/ADD et nettoyage DELETE"
grpc get a tenant-a 100
grpc add a tenant-a 100 "/proc/$NS_A/ns/net"
for instance in a b c; do
    vlan=100
    network=tenant-a
    [[ "$instance" == c ]] && vlan=200 && network=tenant-b
    grpc delete "$instance" "$network" "$vlan"
done

if [[ -d "$STATE_DIR/workloads" ]] && find "$STATE_DIR/workloads" -type f -name '*.json' -print -quit | grep -q .; then
    fail "l'état des workloads n'a pas été nettoyé"
fi
echo "[7/7] OK - intégration CNI complète réussie"
