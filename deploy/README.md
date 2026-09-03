# Deploying Barenetes

`install.sh` is a plug-and-play installer: it downloads the right binaries
from the project's [GitHub Releases](https://github.com/do-2k25-28/Barenetes/releases)
(published by `.github/workflows/release.yml` on every `vX.Y.Z` tag), drops
matching systemd units under `/etc/systemd/system/`, and enables/starts them.
It's idempotent, safe to re-run.

## Control plane

```sh
sudo ./deploy/install.sh --role control-plane
```

Installs and starts `barenetes-api` and `barenetes-scheduler`. By default the
API server uses an **in-memory store** (state is lost on restart) and the
scheduler points at `http://127.0.0.1:50052`.

For persistent storage, either let the installer manage a local etcd:

```sh
sudo ./deploy/install.sh --role control-plane --with-etcd
```

or point at an existing one:

```sh
sudo ./deploy/install.sh --role control-plane --etcd-endpoints http://etcd.internal:2379
```

## Worker nodes

Requires `containerd` already installed and running, and `iptables`
installed (`barenetes-cni` shells out to it for bridge/NAT/firewall rules).
The installer checks both and refuses to proceed otherwise — see below for
how to install containerd; `iptables` is a standard package
(`apt install iptables`, already present on most distros other than a
minimal Debian image).

```sh
sudo ./deploy/install.sh --role worker --server http://192.168.1.10:50052 \
  --node-id 1 \
  --remote-node-ips 192.168.1.12,192.168.1.13 \
  --remote-node-ids 2,3
```

Installs and starts `barenetes-cni` and `barenetes-agent`. `--server` is
**required** here: the agent has no default for where the API server is, and
a worker is by definition not the control-plane host. `--node-id`,
`--remote-node-ips` and `--remote-node-ids` are only needed for multi-node
routing; omit them for a single-node setup. The IDs and IPs must be paired in
the same order.

### Installing containerd

Using Docker's apt repository (see the
[official docs](https://docs.docker.com/engine/install/debian/#install-using-the-repository)):

```sh
sudo apt update
sudo apt install ca-certificates curl
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc

sudo tee /etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/debian
Suites: $(. /etc/os-release && echo "$VERSION_CODENAME")
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

sudo apt update
sudo apt install containerd.io
sudo systemctl enable --now containerd
```

## Single-node (all-in-one) setup

```sh
sudo ./deploy/install.sh --role all
```

Installs all four components on one host. The agent binds to
`127.0.0.1:50053` by default (distinct from the API server's `:50052`) so
there's no port clash. Still requires containerd (see above).

## Options

Run `./deploy/install.sh --help` for the full flag reference (`--version`,
`--repo`, `--server`, etc.).

## What it doesn't do

- Doesn't manage TLS/auth in front of the API server — everything here is
  plaintext gRPC on loopback/LAN, matching the project's current state.
- Doesn't reconcile against containers already running on a worker — the
  agent applies `RUN`/`STOP` events as they arrive, but the opening snapshot
  isn't diffed against local state, so a container left behind by a previous
  agent process isn't cleaned up.
- Doesn't configure firewalling/NAT beyond what `barenetes-cni` itself sets
  up for pod networking.

## Manual reference

Unit files live in `deploy/systemd/` and can be installed by hand if you'd
rather not use the script — copy the ones you need to
`/etc/systemd/system/`, drop the matching env file in `/etc/barenetes/`
(see each unit's `EnvironmentFile=`), then `systemctl daemon-reload &&
systemctl enable --now <unit>`.
