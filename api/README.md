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
