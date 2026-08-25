// Uniform per-RPC tracing: every trait method in `service.rs` routes its handler
// call through here so all 12 RPCs get the same span/event shape.
use std::time::Instant;

use tonic::{Response, Status};
use tracing::Instrument;

pub(crate) async fn traced<F, T>(method: &'static str, fut: F) -> Result<Response<T>, Status>
where
    F: std::future::Future<Output = Result<Response<T>, Status>>,
{
    let start = Instant::now();
    let span = tracing::info_span!("rpc", method);
    async move {
        let result = fut.await;
        let duration = start.elapsed();
        match &result {
            Ok(_) => tracing::info!(?duration, status = "ok", "completed"),
            Err(status) => tracing::warn!(?duration, status = %status.code(), "failed"),
        }
        result
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use tonic::{Code, Response, Status};

    use super::traced;

    #[tokio::test]
    async fn test_traced_passes_through_ok() {
        let result = traced("test_method", async { Ok(Response::new(42)) }).await;
        assert_eq!(result.unwrap().into_inner(), 42);
    }

    #[tokio::test]
    async fn test_traced_passes_through_err() {
        let result: Result<Response<()>, Status> =
            traced("test_method", async { Err(Status::not_found("nope")) }).await;
        let status = result.unwrap_err();
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "nope");
    }
}
