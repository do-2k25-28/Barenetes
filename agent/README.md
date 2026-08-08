

## Build

From the repository root:

```sh
cargo build -p agent
```

## Run

```sh
sudo target/debug/agent
# Kubelet service starting on 127.0.0.1:50052
```

## Build

From the repository root:

```sh
cargo build -p agent
```

## Run

```sh
sudo target/debug/agent
# Kubelet service starting on 127.0.0.1:50052
```

## Start an nginx pod

In another terminal (the client does not need root):

```sh
cargo run -p agent --example kubelet_cli -- apply web docker.io/library/nginx:alpine
# applied pod default-web
```

The first run also pulls the image, so it takes a while later runs are quick.

Check that it is running, the agent puts everything in the `barenetes`
containerd namespace, and names the container `<pod-id>-<index>`:

```sh
sudo ctr -n barenetes containers ls         # default-web-0
sudo ctr -n barenetes tasks ls              # works without root
```

### Curl it

There is no CNI yet, so the pod runs in a private network namespace with only
`lo` nginx is listening on port 80 in there, but it has no address reachable
from the host. Curl it by entering the container's network namespace:

```sh
pgrep -f 'nginx: master' (will outputs a pid)

sudo nsenter -t <output_pid_from_the_previous_command> -n curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1/
# should outputs 200
```

Then delete it:

```sh
cargo run -p agent --example kubelet_cli -- delete default-web
# deleted: true
```


