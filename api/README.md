# api

A gRPC server implementing `service ApiServer` from
`proto/api/v1/api.proto`. Backed by an in-memory store (no database yet -> TODO).

## Running it

```
cargo run -p api
```

Starts listening on `127.0.0.1:50052`. (The scheduler crate uses `50051`, so both can run at the
same time locally.)

## RPCs

`delete_pod`, `watch_pods`, `watch_nodes`, `assign_pod`, and `watch_desired_state`
are still unimplemented (`Status::unimplemented`). Everything else is implemented.
TODO: replace the body of the remaining `*_impl` methods in each file. The routing
in `src/service.rs` is already wired up and shouldn't need to change.
