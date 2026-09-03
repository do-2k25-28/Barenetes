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
scheduler points at `https://127.0.0.1:50052`.

The installer always sets up a private cluster CA and runs `api` and
`scheduler` under mTLS; there's no plaintext opt-out for the control plane.
See [mTLS / cluster PKI](#mtls--cluster-pki) below for what gets generated
and how to bring a worker node into the same trust domain.

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
sudo ./deploy/install.sh --role worker --server https://192.168.1.10:50052 \
  --node-name worker-1 --node-id 1 --node-ip 192.168.1.11 \
  --remote-node-ips 192.168.1.10,192.168.1.12
```

Installs and starts `barenetes-cni` and `barenetes-agent`. `--server` and
`--node-name` are both **required** here: the agent has no default for
where the API server is (a worker is by definition not the control-plane
host), and `--node-name` becomes this node's identity (the CN of its mTLS
certificate, once it's wired up to one). `--node-id`, `--node-ip`, and
`--remote-node-ips` are only needed for the multi-node VXLAN overlay, and
can be omitted for a single-node setup.

By default the agent starts without TLS, which means it can't authenticate
to a control plane's API server: `--role control-plane` always runs `api`
under mandatory mTLS, so an agent with no certificate will just retry and
fail to connect. Add `--ca-dir` to give it one, see
[mTLS / cluster PKI](#mtls--cluster-pki).

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
sudo ./deploy/install.sh --role all --node-name demo-node
```

Installs all four components on one host. The agent binds to
`127.0.0.1:50053` by default (distinct from the API server's `:50052`) so
there's no port clash. Still requires containerd (see above). `--node-name`
is still required (the worker half needs it), but unlike a separate
`--role worker` host, `--ca-dir` isn't needed here: the control plane's CA
was just generated locally, so the agent's certificate is issued from it
directly.

## Options

Run `./deploy/install.sh --help` for the full flag reference (`--version`,
`--repo`, `--server`, etc.).

## mTLS / cluster PKI

`--role control-plane` (and the control-plane half of `--role all`) always
provisions a private CA and runs `barenetes-api`/`barenetes-scheduler`
under mutual TLS, closing off the plaintext gRPC endpoint that used to
accept unauthenticated pod/node operations from anyone on the network. The
CA and the certificates it issues live under `/etc/barenetes/pki/`:

- `pki/ca/ca.pem`, `pki/ca/ca-key.pem`: the cluster CA. Generated once
  (`barenetes-pki init-ca`); re-running the installer doesn't touch it.
  **`ca-key.pem` never leaves the control-plane host.**
- `pki/issued/api.pem`, `pki/issued/scheduler.pem` (plus their `-key.pem`
  files): leaf certificates for the two control-plane services, issued
  with `barenetes-pki issue`.

`barenetes-api.service`/`barenetes-scheduler.service` run under
`DynamicUser=true`, so they can't read the 0600 key files directly; the
units instead use systemd's `LoadCredential=` to hand those files to their
transient user at start (see the units for details).

A worker's agent stays plaintext by default. To give it a certificate:

- **Same host as the control plane** (`--role all`): nothing extra to
  do, the installer issues the node's certificate from the local CA
  automatically.
- **A separate worker host**: the CA private key must never be copied
  there. On the control-plane host, issue the node's certificate against
  the CA's public half:

  ```sh
  sudo barenetes-pki issue --ca-dir /etc/barenetes/pki/ca \
    --cn worker-1 --out-dir /tmp/worker-1-pki
  ```

  Copy `/etc/barenetes/pki/ca/ca.pem` and the two files from
  `/tmp/worker-1-pki/` to the worker (e.g. `scp`) into one directory, then
  point the installer at it:

  ```sh
  sudo ./deploy/install.sh --role worker --node-name worker-1 \
    --ca-dir /path/to/that/directory
  ```

Certificate rotation isn't automated; re-issuing and restarting the
affected service is a manual step for now.

## What it doesn't do

- Doesn't reconcile against containers already running on a worker — the
  agent applies `RUN`/`STOP` events as they arrive, but the opening snapshot
  isn't diffed against local state, so a container left behind by a previous
  agent process isn't cleaned up.
- Doesn't rotate certificates, or provide a pod-to-pod service mesh; mTLS
  covers the api/scheduler/barectl and agent/api RPC links, not a mesh
  between pods themselves (see `docs/mtls-plan.md`).
- Doesn't configure firewalling/NAT beyond what `barenetes-cni` itself sets
  up for pod networking.

## Manual reference

Unit files live in `deploy/systemd/` and can be installed by hand if you'd
rather not use the script: copy the ones you need to
`/etc/systemd/system/`, drop the matching env file in `/etc/barenetes/`
(see each unit's `EnvironmentFile=`), then `systemctl daemon-reload &&
systemctl enable --now <unit>`.
