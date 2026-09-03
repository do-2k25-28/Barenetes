

## Build

From the repository root:

```sh
cargo build -p agent
```

## Run

```sh
sudo target/debug/agent
# Kubelet service starting on 127.0.0.1:50053
```

Override the bind address with `--addr` or `BARENETES_AGENT_ADDR`:

```sh
sudo target/debug/agent --addr 127.0.0.1:60053
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

## Resource limits

`kubelet_cli apply` takes optional `--cpu` and `--memory` flags:

```sh
cargo run -p agent --example kubelet_cli -- apply web docker.io/library/nginx:alpine --cpu 500 --memory 256
```

`--cpu` is in millicores. 1000 means one full core, 500 means half a core.
`--memory` is in megabytes. Leave a flag out to leave that resource
unlimited.

The agent turns these into a cgroup for the container. A CPU limit is set as
a quota over a period: the container gets `quota` microseconds of CPU time
out of every `period` microseconds. The agent uses a 100ms period, so the
quota is `cpu * 100000 / 1000`.

The kernel rejects any quota below 1000 microseconds. Limits under 10
millicores would compute a smaller quota than that, so the agent rounds
them up to 1000 microseconds instead of letting the container fail to
start.

## Using a generic gRPC client instead of `kubelet_cli`

`kubelet_cli` is only a convenience wrapper, the kubelet exposes a plain gRPC
service (`agent.v1.Kubelet`). If you already have a gRPC client installed
([grpcurl](https://github.com/fullstorydev/grpcurl), Postman) you can call the service
directly, otherwise stick to `kubelet_cli`, it needs no extra tooling.

Start the same nginx pod as above:

```sh
grpcurl -plaintext \
  -import-path proto -proto agent/v1/kubelet.proto \
  -d '{
        "pod": {
          "pod": { "name": "web" },
          "spec": {
            "namespace": "default",
            "containers": [
              { "name": "0", "image": "docker.io/library/nginx:alpine" }
            ]
          }
        }
      }' \
  127.0.0.1:50053 agent.v1.Kubelet/ApplyPod
# { "podId": "default-web" }
```

And delete it:

```sh
grpcurl -plaintext \
  -import-path proto -proto agent/v1/kubelet.proto \
  -d '{ "podId": "default-web", "gracePeriodSeconds": 5, "force": false }' \
  127.0.0.1:50053 agent.v1.Kubelet/DeletePod
# { "success": true }
```


