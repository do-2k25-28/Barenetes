#!/usr/bin/env bash
# Barenetes uninstaller. Reverses what deploy/install.sh did: stops/disables
# and removes systemd units, removes installed binaries, and (opt-in) purges
# config and persisted state. Safe to re-run.
#
# Usage:
#   sudo ./deploy/uninstall.sh [options]
#
# Options:
#   --role control-plane|worker|all|etcd   Components to remove (default: all)
#   --purge                                Also remove /etc/barenetes (config) and
#                                           /var/lib/barenetes, /var/lib/etcd (state) --
#                                           irreversible: drops etcd data, CNI IP pool
#                                           allocations, and agent VLAN allocations
#   -h, --help                             Show this help and exit
#
# `--role all` (the default) removes every Barenetes component found on the
# host, including a locally-installed etcd. Components that were never
# installed are silently skipped, so this is safe to run unconditionally.
set -euo pipefail

ROLE="all"
PURGE=false
PREFIX="/usr/local/bin"
CONF_DIR="/etc/barenetes"
STATE_DIR="/var/lib/barenetes"
ETCD_STATE_DIR="/var/lib/etcd"
UNIT_DIR="/etc/systemd/system"

REMOVED_UNITS=()

log()  { printf '==> %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Barenetes uninstaller.

Reverses what deploy/install.sh did: stops/disables and removes systemd
units, removes installed binaries, and (opt-in) purges config and persisted
state. Safe to re-run.

Usage:
  sudo ./deploy/uninstall.sh [options]

Options:
  --role control-plane|worker|all|etcd   Components to remove (default: all)
  --purge                                Also remove /etc/barenetes (config) and
                                          /var/lib/barenetes, /var/lib/etcd (state) --
                                          irreversible: drops etcd data, CNI IP pool
                                          allocations, and agent VLAN allocations
  -h, --help                             Show this help and exit

`--role all` (the default) removes every Barenetes component found on the
host, including a locally-installed etcd. Components that were never
installed are silently skipped, so this is safe to run unconditionally.
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --role) ROLE="$2"; shift 2 ;;
      --purge) PURGE=true; shift ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done

  case "$ROLE" in
    control-plane|worker|all|etcd) ;;
    *) die "--role must be one of: control-plane, worker, all, etcd" ;;
  esac
}

require_root() {
  [[ "${EUID:-$(id -u)}" -eq 0 ]] || die "must run as root (try: sudo $0 ...)"
}

require_systemd() {
  command -v systemctl >/dev/null 2>&1 \
    || die "systemctl not found; this uninstaller targets systemd-based distros"
}

# Stops, disables, and removes a unit if it's actually installed. Silently
# skipping absent units (rather than erroring) is what makes --role all safe
# to run on a host that only ever had, say, --role worker installed.
remove_unit() {
  local unit="$1" path="${UNIT_DIR}/${1}"

  [[ -f "$path" ]] || return 0

  log "Stopping and removing ${unit}..."
  systemctl stop "$unit" 2>/dev/null || true
  systemctl disable "$unit" 2>/dev/null || true
  rm -f "$path"
  REMOVED_UNITS+=("$unit")
}

remove_binary() {
  local path="${PREFIX}/${1}"
  if [[ -e "$path" ]]; then
    log "Removing ${path}..."
    rm -f "$path"
  fi
}

uninstall_control_plane() {
  log "Removing control-plane components: api, scheduler"
  remove_unit barenetes-api.service
  remove_unit barenetes-scheduler.service
  remove_binary barenetes-api
  remove_binary barenetes-scheduler
  rm -f "${CONF_DIR}/api.env" "${CONF_DIR}/scheduler.env"
}

uninstall_worker() {
  log "Removing worker components: cni, agent"
  remove_unit barenetes-agent.service
  remove_unit barenetes-cni.service
  remove_binary barenetes-agent
  remove_binary barenetes-cni
  rm -f "${CONF_DIR}/agent.env" "${CONF_DIR}/cni.env"
  remove_network_state
}

# barenetes-cni manages Linux bridge/VXLAN/VLAN interfaces and iptables
# chains directly (cni/src/network/{bridge,overlay,vlan,firewall}.rs) --
# state that lives in the kernel, outside its systemd State/RuntimeDirectory,
# and isn't cleaned up by removing the unit and binary. Best-effort: a host
# that never ran barenetes-cni just has nothing here to remove.
remove_network_state() {
  command -v ip >/dev/null 2>&1 || return 0

  log "Removing barenetes-cni network state..."

  # Per-workload veths (host end "v<10 hex>"; deleting it takes the peer
  # "p<10 hex>" with it). Normally torn down on DeleteWorkloadNetwork --
  # only left behind if the daemon crashed mid-lifecycle.
  local iface
  while read -r iface; do
    [[ "$iface" =~ ^v[0-9a-f]{10}$ ]] || continue
    log "  removing orphaned veth ${iface}..."
    ip link delete "$iface" 2>/dev/null || true
  done < <(ip -o link show | awk -F': ' '{print $2}' | cut -d@ -f1)

  # Per-tenant VLAN sub-interfaces (barenetes0.<vlan-id>).
  while read -r iface; do
    log "  removing VLAN sub-interface ${iface}..."
    ip link delete "$iface" 2>/dev/null || true
  done < <(ip -o link show | awk -F': ' '{print $2}' | cut -d@ -f1 | grep -E '^barenetes0\.[0-9]+$')

  if ip link show barenetes-vx >/dev/null 2>&1; then
    log "  removing VXLAN interface barenetes-vx..."
    ip link delete barenetes-vx 2>/dev/null || true
  fi
  if ip link show barenetes0 >/dev/null 2>&1; then
    log "  removing bridge barenetes0..."
    ip link delete barenetes0 2>/dev/null || true
  fi

  # Jump rules must go before the chains they reference, or -X fails with
  # "Chain is in use". Match args mirror ensure_egress() in
  # cni/src/network/firewall.rs exactly (OUTPUT's jump is scoped by
  # --dst-type LOCAL; PREROUTING/FORWARD's are not).
  if command -v iptables >/dev/null 2>&1; then
    iptables -t nat -D PREROUTING -j BARENETES-PREROUTING 2>/dev/null || true
    iptables -t nat -F BARENETES-PREROUTING 2>/dev/null || true
    iptables -t nat -X BARENETES-PREROUTING 2>/dev/null || true

    iptables -t nat -D OUTPUT -m addrtype --dst-type LOCAL -j BARENETES-OUTPUT 2>/dev/null || true
    iptables -t nat -F BARENETES-OUTPUT 2>/dev/null || true
    iptables -t nat -X BARENETES-OUTPUT 2>/dev/null || true

    iptables -t filter -D FORWARD -j BARENETES-FORWARD 2>/dev/null || true
    iptables -t filter -F BARENETES-FORWARD 2>/dev/null || true
    iptables -t filter -X BARENETES-FORWARD 2>/dev/null || true
  fi

  # ensure_egress() sets this on every barenetes-cni start; left enabled
  # since disabling forwarding globally could affect unrelated services.
  log "  note: net.ipv4.ip_forward=1 (set by barenetes-cni) was left as-is; reset manually if desired."
}

# etcd is orthogonal to --role: install.sh only drops it in when
# --with-etcd was passed, regardless of control-plane/worker/all. Detecting
# by unit presence (rather than tracking install-time flags here) is what
# lets a bare `uninstall.sh` clean it up without the caller having to
# remember how it was installed.
uninstall_etcd() {
  log "Removing etcd"
  remove_unit etcd.service
  remove_binary etcd
  remove_binary etcdctl
  remove_binary etcdutl
}

purge_state() {
  [[ "$PURGE" == true ]] || return 0

  log "Purging config (${CONF_DIR})..."
  rm -rf "$CONF_DIR"

  log "Purging state (${STATE_DIR})..."
  rm -rf "$STATE_DIR"

  if [[ -d "$ETCD_STATE_DIR" ]]; then
    log "Purging etcd data (${ETCD_STATE_DIR})..."
    rm -rf "$ETCD_STATE_DIR"
  fi
}

main() {
  parse_args "$@"
  require_root
  require_systemd

  case "$ROLE" in
    control-plane) uninstall_control_plane ;;
    worker) uninstall_worker ;;
    etcd) uninstall_etcd ;;
    all)
      uninstall_control_plane
      uninstall_worker
      uninstall_etcd
      ;;
  esac

  systemctl daemon-reload

  purge_state

  if [[ "${#REMOVED_UNITS[@]}" -eq 0 ]]; then
    log "Nothing to remove: no matching units were installed."
  else
    log "Done. Removed units: ${REMOVED_UNITS[*]}"
  fi

  if [[ "$PURGE" != true ]]; then
    log "Config (${CONF_DIR}) and state (${STATE_DIR}, ${ETCD_STATE_DIR}) were left in place. Re-run with --purge to remove them."
  fi
}

main "$@"
