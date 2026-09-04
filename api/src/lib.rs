//! Library half of the `api` crate: `main.rs` is a thin binary wrapper
//! around this. Split out so `tests/` integration tests (which exercise the
//! server over a real TLS listener) can build an [`service::ApiService`]
//! without going through the CLI.
mod errors;
mod handlers;
pub mod service;
pub mod store;
mod telemetry;
#[cfg(test)]
mod test_support;
mod validation;
