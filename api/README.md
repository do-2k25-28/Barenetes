# BARENETES API

A gRPC server implementing `service ApiServer` from
`proto/api/v1/api.proto`. Backed by etcd for persistent storage,
with an in-memory fallback for testing.

## Running it

Without etcd (in-memory, for development/testing):
```
cargo run -p api
```

With etcd (persistent storage):
```
BARENETES_ETCD_ENDPOINTS=http://127.0.0.1:2379 cargo run -p api
```

Listens on `127.0.0.1:50052` by default. For a real multi-node deployment,
worker agents need to reach this over the network, so override it:
```
BARENETES_LISTEN_ADDR=0.0.0.0:50052 cargo run -p api
```

**No authentication or TLS**: every RPC (create/delete pods, register nodes,
...) is open to anyone who can reach the listen address, in plaintext. This
is fine on loopback. Once you set `BARENETES_LISTEN_ADDR` to anything else,
you are responsible for restricting reachability yourself -- a private
overlay network, a firewall, a VPN -- never expose it on a public interface.
The server logs a warning at startup if it detects a non-loopback address.
