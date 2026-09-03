#!/usr/bin/env bash
# Barenetes plug-and-play installer.
#
# Installs binaries from a GitHub Release, drops matching systemd units,
# and enables/starts the requested components. Safe to re-run (idempotent).
#
# Usage:
#   sudo ./deploy/install.sh [options]
#   curl -fsSL https://raw.githubusercontent.com/do-2k25-28/Barenetes/main/deploy/install.sh \
#     | sudo bash -s -- [options]
#
# Options:
#   --role control-plane|worker|all   Components to install (default: control-plane)
#   --version TAG                     Release tag to install, e.g. v0.1.0 (default: latest)
#   --repo OWNER/NAME                 GitHub repo to fetch releases from (default: do-2k25-28/Barenetes)
#   --local-dist DIR                  Install binaries from a local dir instead of GitHub (offline/air-gapped;
#                                      expects barenetes-<name>-linux-x86_64 files)
#   --with-etcd                       Install and run a local etcd for the API server's store
#   --etcd-endpoints URL[,URL...]     Point the API server at an existing etcd instead
#   --server URL                      API server address for the scheduler and agent
#                                      (required for --role worker; default:
#                                      https://127.0.0.1:50052 for control-plane/all)
#   --node-id N                       CNI BARENETES_NODE_ID, 0-255 (worker/all; default: 0)
#   --node-ip IP                      CNI BARENETES_NODE_IP (worker/all, for multi-node VXLAN overlay)
#   --remote-node-ips IP[,IP...]      CNI BARENETES_REMOTE_NODE_IPS (worker/all, for multi-node overlay)
#   --node-name NAME                  This node's identity (worker/all; required). Becomes the CN of
#                                      the agent's mTLS certificate.
#   --ca-dir DIR                      Directory holding ca.pem plus a pre-issued <node-name>.pem/
#                                      <node-name>-key.pem for this node (worker only; see
#                                      deploy/README.md). Not needed for --role all, which issues the
#                                      node's certificate itself from the CA it just generated.
#   -h, --help                        Show this help and exit
#
# --role all installs both control-plane and worker components on the same
# host (e.g. a single-node dev/demo box). The agent binds 127.0.0.1:50053 by
# default specifically so it doesn't collide with the API server's :50052.
#
# The control plane always sets up a private cluster CA and runs the API
# server and scheduler under mTLS; there's no plaintext opt-out. Worker
# nodes stay plaintext unless --ca-dir wires up the agent's certificate (or
# --role all, which needs no --ca-dir since it holds the CA locally).
set -euo pipefail

REPO="do-2k25-28/Barenetes"
VERSION="latest"
LOCAL_DIST_DIR=""
ROLE="control-plane"
WITH_ETCD=false
ETCD_ENDPOINTS=""
SERVER="https://127.0.0.1:50052"
SERVER_SET=false
NODE_ID=""
NODE_IP=""
NODE_NAME=""
CA_DIR=""
REMOTE_NODE_IPS=""
PREFIX="/usr/local/bin"
CONF_DIR="/etc/barenetes"
UNIT_DIR="/etc/systemd/system"
ETCD_VERSION="v3.7.1"
# Fixed (not overridable): the barenetes-api/-scheduler systemd units
# hardcode these same paths in their LoadCredential= lines, so they can't
# move independently of CONF_DIR.
PKI_CA_DIR="${CONF_DIR}/pki/ca"
PKI_ISSUED_DIR="${CONF_DIR}/pki/issued"

INSTALLED_UNITS=()

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd -P || true)"

log()  { printf '==> %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  # A heredoc rather than self-reading the script: piping the script into
  # `bash -s` (curl | sudo bash -s -- --help) leaves no readable source
  # file for `${BASH_SOURCE[0]}` to point at.
  cat <<'EOF'
Barenetes plug-and-play installer.

Installs binaries from a GitHub Release, drops matching systemd units,
and enables/starts the requested components. Safe to re-run (idempotent).

Usage:
  sudo ./deploy/install.sh [options]
  curl -fsSL https://raw.githubusercontent.com/do-2k25-28/Barenetes/main/deploy/install.sh \
    | sudo bash -s -- [options]

Options:
  --role control-plane|worker|all   Components to install (default: control-plane)
  --version TAG                     Release tag to install, e.g. v0.1.0 (default: latest)
  --repo OWNER/NAME                 GitHub repo to fetch releases from (default: do-2k25-28/Barenetes)
  --local-dist DIR                  Install binaries from a local dir instead of GitHub (offline/air-gapped;
                                     expects barenetes-<name>-linux-x86_64 files)
  --with-etcd                       Install and run a local etcd for the API server's store
  --etcd-endpoints URL[,URL...]     Point the API server at an existing etcd instead
  --server URL                      API server address for the scheduler and agent
                                     (required for --role worker; default:
                                     https://127.0.0.1:50052 for control-plane/all)
  --node-id N                       CNI BARENETES_NODE_ID, 0-255 (worker/all; default: 0)
  --node-ip IP                      CNI BARENETES_NODE_IP (worker/all, for multi-node VXLAN overlay)
  --remote-node-ips IP[,IP...]      CNI BARENETES_REMOTE_NODE_IPS (worker/all, for multi-node overlay)
  --node-name NAME                  This node's identity (worker/all; required). Becomes the CN of
                                     the agent's mTLS certificate.
  --ca-dir DIR                      Directory holding ca.pem plus a pre-issued <node-name>.pem/
                                     <node-name>-key.pem for this node (worker only; see
                                     deploy/README.md). Not needed for --role all, which issues the
                                     node's certificate itself from the CA it just generated.
  -h, --help                        Show this help and exit

--role all installs both control-plane and worker components on the same
host (e.g. a single-node dev/demo box). The agent binds 127.0.0.1:50053 by
default specifically so it doesn't collide with the API server's :50052.

The control plane always sets up a private cluster CA and runs the API
server and scheduler under mTLS; there's no plaintext opt-out. Worker
nodes stay plaintext unless --ca-dir wires up the agent's certificate (or
--role all, which needs no --ca-dir since it holds the CA locally).
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --role) ROLE="$2"; shift 2 ;;
      --version) VERSION="$2"; shift 2 ;;
      --repo) REPO="$2"; shift 2 ;;
      --local-dist) LOCAL_DIST_DIR="$2"; shift 2 ;;
      --with-etcd) WITH_ETCD=true; shift ;;
      --etcd-endpoints) ETCD_ENDPOINTS="$2"; shift 2 ;;
      --server) SERVER="$2"; SERVER_SET=true; shift 2 ;;
      --node-id) NODE_ID="$2"; shift 2 ;;
      --node-ip) NODE_IP="$2"; shift 2 ;;
      --remote-node-ips) REMOTE_NODE_IPS="$2"; shift 2 ;;
      --node-name) NODE_NAME="$2"; shift 2 ;;
      --ca-dir) CA_DIR="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done

  case "$ROLE" in
    control-plane|worker|all) ;;
    *) die "--role must be one of: control-plane, worker, all" ;;
  esac

  # A worker is by definition not the control-plane host, so the loopback
  # default can never be right there -- the agent would sit retrying against
  # nothing forever. --role all is a single-box install, where it is right.
  if [[ "$ROLE" == "worker" && "$SERVER_SET" != true ]]; then
    die "--role worker requires --server URL (the control-plane API server, e.g. --server http://192.168.1.10:50052)"
  fi
}

require_root() {
  [[ "${EUID:-$(id -u)}" -eq 0 ]] || die "must run as root (try: sudo $0 ...)"
}

require_systemd() {
  command -v systemctl >/dev/null 2>&1 \
    || die "systemctl not found; this installer targets systemd-based distros"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# `enable --now` is a no-op start on a unit that's already running, so a
# re-run that only changed the env file (e.g. new CNI overlay params)
# wouldn't actually pick it up. Always restart explicitly instead.
enable_and_restart() {
  systemctl enable "$1"
  systemctl restart "$1"
}

# "latest" isn't a valid git ref for raw.githubusercontent.com, so unit
# files fetched over the network (no local checkout) need a concrete tag.
# Binary downloads avoid this entirely via the /releases/latest/download/
# redirect, so this is only called on that fallback path.
resolve_tag() {
  if [[ "$VERSION" != "latest" ]]; then
    printf '%s' "$VERSION"
    return
  fi
  # Buffer the response before grep/sed: piping curl straight into a
  # `grep -m1` that exits after the first match can make curl see a
  # broken pipe (SIGPIPE) on a large response, which trips `pipefail`.
  local response
  response="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")" \
    || die "failed to query https://api.github.com/repos/${REPO}/releases/latest"
  grep -m1 '"tag_name"' <<<"$response" | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/'
}

install_binary() {
  local name="$1" # agent | api | barectl | cni | scheduler
  local asset="barenetes-${name}-linux-x86_64"
  local url tmp

  if [[ -n "$LOCAL_DIST_DIR" ]]; then
    log "Installing ${asset} from ${LOCAL_DIST_DIR} (offline/--local-dist)..."
    [[ -f "${LOCAL_DIST_DIR}/${asset}" ]] || die "local dist file not found: ${LOCAL_DIST_DIR}/${asset}"
    install -m 0755 "${LOCAL_DIST_DIR}/${asset}" "${PREFIX}/barenetes-${name}"
    return
  fi

  if [[ "$VERSION" == "latest" ]]; then
    url="https://github.com/${REPO}/releases/latest/download/${asset}"
  else
    url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
  fi

  log "Downloading ${asset} (${VERSION})..."
  tmp="$(mktemp)"
  curl -fsSL "$url" -o "$tmp" || { rm -f "$tmp"; die "failed to download ${url}"; }
  install -m 0755 "$tmp" "${PREFIX}/barenetes-${name}"
  rm -f "$tmp"
}

# Prefers the unit file from a local checkout (script run as
# ./deploy/install.sh from a git clone); falls back to fetching it from
# GitHub for the curl-pipe-bash case.
fetch_unit_file() {
  local name="$1" dest="$2" local_path tag

  local_path="${SCRIPT_DIR}/systemd/${name}"
  if [[ -n "$SCRIPT_DIR" && -f "$local_path" ]]; then
    install -m 0644 "$local_path" "$dest"
    return
  fi

  tag="$(resolve_tag)"
  log "Fetching ${name} from ${REPO}@${tag}..."
  curl -fsSL "https://raw.githubusercontent.com/${REPO}/${tag}/deploy/systemd/${name}" -o "$dest" \
    || die "failed to fetch systemd unit ${name} for ${REPO}@${tag}"
  chmod 0644 "$dest"
}

install_etcd() {
  local url tmp_dir
  url="https://github.com/etcd-io/etcd/releases/download/${ETCD_VERSION}/etcd-${ETCD_VERSION}-linux-amd64.tar.gz"

  log "Installing etcd ${ETCD_VERSION}..."
  tmp_dir="$(mktemp -d)"
  curl -fsSL "$url" -o "${tmp_dir}/etcd.tar.gz" || die "failed to download etcd from ${url}"
  tar xzf "${tmp_dir}/etcd.tar.gz" -C "$tmp_dir" --strip-components=1
  install -m 0755 "${tmp_dir}/etcd" "${tmp_dir}/etcdctl" "${tmp_dir}/etcdutl" "${PREFIX}/"
  rm -rf "$tmp_dir"

  fetch_unit_file "etcd.service" "${UNIT_DIR}/etcd.service"
  systemctl daemon-reload
  enable_and_restart etcd.service
  INSTALLED_UNITS+=("etcd.service")
}

write_conf() {
  local path="$1"
  install -d -m 0755 "$CONF_DIR"
  cat > "$path"
}

# Generates the cluster CA on first run (idempotent: `barenetes-pki init-ca`
# itself refuses to overwrite an existing ca.pem) and issues the api/
# scheduler leaf certs the control plane's own two services use.
ensure_control_plane_pki() {
  install_binary pki
  install -d -m 0755 "$PKI_ISSUED_DIR"

  if [[ ! -f "${PKI_CA_DIR}/ca.pem" ]]; then
    log "Generating cluster CA at ${PKI_CA_DIR}..."
    "${PREFIX}/barenetes-pki" init-ca --out-dir "$PKI_CA_DIR"
  fi
  if [[ ! -f "${PKI_ISSUED_DIR}/api.pem" ]]; then
    "${PREFIX}/barenetes-pki" issue --ca-dir "$PKI_CA_DIR" --cn api --out-dir "$PKI_ISSUED_DIR"
  fi
  if [[ ! -f "${PKI_ISSUED_DIR}/scheduler.pem" ]]; then
    "${PREFIX}/barenetes-pki" issue --ca-dir "$PKI_CA_DIR" --cn scheduler --out-dir "$PKI_ISSUED_DIR"
  fi
}

install_control_plane() {
  log "Installing control-plane components: api, scheduler"

  ensure_control_plane_pki

  if [[ -n "$ETCD_ENDPOINTS" ]]; then
    write_conf "${CONF_DIR}/api.env" <<-EOF
			RUST_LOG=info
			BARENETES_API_ADDR=0.0.0.0:50052
			BARENETES_ETCD_ENDPOINTS=${ETCD_ENDPOINTS}
			EOF
  elif [[ "$WITH_ETCD" == true ]]; then
    install_etcd
    write_conf "${CONF_DIR}/api.env" <<-EOF
			RUST_LOG=info
			BARENETES_API_ADDR=0.0.0.0:50052
			BARENETES_ETCD_ENDPOINTS=http://127.0.0.1:2379
			EOF
  else
    write_conf "${CONF_DIR}/api.env" <<-EOF
			RUST_LOG=info
			BARENETES_API_ADDR=0.0.0.0:50052
			# In-memory store: state is lost on restart. Re-run with --with-etcd
			# or --etcd-endpoints for persistence.
			EOF
  fi

  # barenetes-api.service itself passes --tls-cert/--tls-key/--tls-ca (via
  # LoadCredential=) on ExecStart=, not through this env file: DynamicUser
  # can't read the 0600 key file directly, and a real path set here would
  # silently beat that unit's own override (EnvironmentFile= always wins
  # over Environment=, regardless of order) if it were duplicated here too.

  install_binary api
  fetch_unit_file barenetes-api.service "${UNIT_DIR}/barenetes-api.service"
  INSTALLED_UNITS+=("barenetes-api.service")

  # Same reasoning as api.env above: barenetes-scheduler.service passes
  # --tls-cert/--tls-key/--tls-ca itself. BARENETES_TLS_SERVER_NAME has no
  # such conflict (it's not a credential file), so it's set here as usual.
  write_conf "${CONF_DIR}/scheduler.env" <<-EOF
		RUST_LOG=info
		BARENETES_SERVER=${SERVER}
		BARENETES_TLS_SERVER_NAME=api
		EOF

  install_binary scheduler
  fetch_unit_file barenetes-scheduler.service "${UNIT_DIR}/barenetes-scheduler.service"
  INSTALLED_UNITS+=("barenetes-scheduler.service")

  systemctl daemon-reload
  enable_and_restart barenetes-api.service
  enable_and_restart barenetes-scheduler.service
}

# Wires up the agent's mTLS certificate, if one is available, and appends
# the resulting BARENETES_TLS_* vars to agent.env. Two cases:
#   - --role all: the CA (with its private key) was just generated locally
#     by install_control_plane(), so the node's cert can be issued here
#     directly.
#   - --role worker (a separate host): the CA private key must never leave
#     the control plane, so --ca-dir instead points at a directory the
#     operator populated by hand with ca.pem plus a cert already issued on
#     the control plane for this node_name.
# Neither case applies (plaintext worker) leaves agent.env unchanged, with
# a warning.
setup_worker_pki() {
  local tls_cert tls_key tls_ca

  if [[ -n "$CA_DIR" ]]; then
    [[ -f "${CA_DIR}/ca.pem" ]] || die "--ca-dir ${CA_DIR} is missing ca.pem"
    [[ -f "${CA_DIR}/${NODE_NAME}.pem" && -f "${CA_DIR}/${NODE_NAME}-key.pem" ]] \
      || die "--ca-dir ${CA_DIR} is missing ${NODE_NAME}.pem/${NODE_NAME}-key.pem for this node. Issue them on the control plane first with: barenetes-pki issue --ca-dir <path-to-ca> --cn ${NODE_NAME} --out-dir <dir>, then copy them here next to ca.pem (never copy ca-key.pem to a worker)."
    # Copied into /etc/barenetes/pki rather than read from --ca-dir in
    # place: that directory is operator-chosen and typically lands under
    # $HOME, which barenetes-agent.service's ProtectHome=true hides from
    # the running service.
    install -d -m 0755 "$PKI_ISSUED_DIR" "$PKI_CA_DIR"
    install -m 0644 "${CA_DIR}/${NODE_NAME}.pem" "${PKI_ISSUED_DIR}/${NODE_NAME}.pem"
    install -m 0600 "${CA_DIR}/${NODE_NAME}-key.pem" "${PKI_ISSUED_DIR}/${NODE_NAME}-key.pem"
    install -m 0644 "${CA_DIR}/ca.pem" "${PKI_CA_DIR}/ca.pem"
    tls_cert="${PKI_ISSUED_DIR}/${NODE_NAME}.pem"
    tls_key="${PKI_ISSUED_DIR}/${NODE_NAME}-key.pem"
    tls_ca="${PKI_CA_DIR}/ca.pem"
  elif [[ -f "${PKI_CA_DIR}/ca-key.pem" ]]; then
    install_binary pki
    install -d -m 0755 "$PKI_ISSUED_DIR"
    if [[ ! -f "${PKI_ISSUED_DIR}/${NODE_NAME}.pem" ]]; then
      "${PREFIX}/barenetes-pki" issue --ca-dir "$PKI_CA_DIR" --cn "$NODE_NAME" --out-dir "$PKI_ISSUED_DIR"
    fi
    tls_cert="${PKI_ISSUED_DIR}/${NODE_NAME}.pem"
    tls_key="${PKI_ISSUED_DIR}/${NODE_NAME}-key.pem"
    tls_ca="${PKI_CA_DIR}/ca.pem"
  else
    log "No --ca-dir given: barenetes-agent will start WITHOUT TLS (unauthenticated, unencrypted). See deploy/README.md to enable mTLS."
    return
  fi

  {
    echo "BARENETES_TLS_CERT=${tls_cert}"
    echo "BARENETES_TLS_KEY=${tls_key}"
    echo "BARENETES_TLS_CA=${tls_ca}"
    # The api server's own cert is always issued CN=api (see
    # ensure_control_plane_pki), so this is the identity the agent's
    # desired-state client must verify api's certificate against.
    echo "BARENETES_TLS_SERVER_NAME=api"
  } >> "${CONF_DIR}/agent.env"
}

install_worker() {
  log "Installing worker components: cni, agent"

  [[ -n "$NODE_NAME" ]] || die "--node-name is required for --role worker/all"

  systemctl is-active --quiet containerd \
    || die "containerd is required on worker nodes but isn't running. Install/start it first (see deploy/README.md), then re-run with --role worker."
  command -v iptables >/dev/null 2>&1 \
    || die "iptables is required by barenetes-cni (bridge/NAT/firewall rules) but isn't installed. Install it (e.g. apt install iptables), then re-run with --role worker."

  install -d -m 0755 "$CONF_DIR"
  {
    echo "RUST_LOG=info"
    [[ -n "$NODE_ID" ]] && echo "BARENETES_NODE_ID=${NODE_ID}"
    [[ -n "$NODE_IP" ]] && echo "BARENETES_NODE_IP=${NODE_IP}"
    [[ -n "$REMOTE_NODE_IPS" ]] && echo "BARENETES_REMOTE_NODE_IPS=${REMOTE_NODE_IPS}"
  } > "${CONF_DIR}/cni.env"

  install_binary cni
  fetch_unit_file barenetes-cni.service "${UNIT_DIR}/barenetes-cni.service"
  INSTALLED_UNITS+=("barenetes-cni.service")

  write_conf "${CONF_DIR}/agent.env" <<-EOF
		RUST_LOG=info
		BARENETES_SERVER=${SERVER}
		BARENETES_NODE_NAME=${NODE_NAME}
		EOF
  setup_worker_pki

  install_binary agent
  fetch_unit_file barenetes-agent.service "${UNIT_DIR}/barenetes-agent.service"
  INSTALLED_UNITS+=("barenetes-agent.service")

  systemctl daemon-reload
  enable_and_restart barenetes-cni.service
  enable_and_restart barenetes-agent.service
}

main() {
  parse_args "$@"
  require_root
  require_systemd
  require_cmd curl
  require_cmd tar
  require_cmd install

  install -d -m 0755 "$CONF_DIR" "$PREFIX"

  case "$ROLE" in
    control-plane) install_control_plane ;;
    worker) install_worker ;;
    all) install_control_plane; install_worker ;;
  esac

  log "Done. Installed units: ${INSTALLED_UNITS[*]}"
  systemctl --no-pager --plain status "${INSTALLED_UNITS[@]}" || true
}

main "$@"
