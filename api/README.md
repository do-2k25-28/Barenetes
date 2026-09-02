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

Starts listening on `127.0.0.1:50052`. Override with `--addr` or `BARENETES_API_ADDR`:
```
cargo run -p api -- --addr 127.0.0.1:60052
```

## RPCs

`delete_pod`, `watch_pods`, `watch_nodes`, `assign_pod`, and `watch_desired_state`
are still unimplemented (`Status::unimplemented`). Everything else is implemented.
TODO: replace the body of the remaining `*_impl` methods in each file. The routing
in `src/service.rs` is already wired up and shouldn't need to change.
