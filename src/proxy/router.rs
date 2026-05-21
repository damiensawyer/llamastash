//! Per-request dispatch. `route` is the body of the `service_fn`
//! closure each hyper connection runs — a flat `match` over
//! `(method, path)` for the six fixed routes the proxy answers,
//! mirroring the style of [`crate::ipc::methods::dispatch_request`].
//!
//! Unit 1 only implements `/health`; every other path returns 501
//! `Not Implemented`. Subsequent units replace the 501 arms with
//! real handlers without touching this file's outer shape.

use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::json;

use super::state::ProxyState;

/// The error type our `BoxBody` carries. We control every body we
/// emit (all in-memory `Bytes`), so an infallible error is the most
/// honest signal — chunks never fail at frame time. When Unit 3
/// starts piping reqwest's `bytes_stream()` through, the body alias
/// switches to a `BoxBody<Bytes, BoxError>` instead.
pub type BodyError = Infallible;

/// What every handler returns. `Result<_, hyper::Error>` is the
/// `service_fn` contract; the inner body is boxed so each arm can
/// pick whatever concrete `Body` makes sense without poisoning the
/// outer signature.
pub type ProxyResponse = Result<Response<BoxBody<Bytes, BodyError>>, hyper::Error>;

/// Entry point invoked by the `service_fn` closure. Returns a fully
/// constructed `Response`; the caller hands it back to hyper.
pub async fn route(state: Arc<ProxyState>, req: Request<Incoming>) -> ProxyResponse {
  let method = req.method().clone();
  let path = req.uri().path().to_string();

  // 6-route dispatch table. The five non-`/health` arms are 501
  // until later units replace them with the real handler bodies.
  // Keeping them named here rather than in a single `_ =>` catch-all
  // documents the surface and makes it obvious which units land
  // where.
  match (&method, path.as_str()) {
    (&Method::GET, "/health") => health(state).await,
    (&Method::GET, "/v1/models") => not_implemented(),
    (&Method::POST, "/v1/chat/completions") => not_implemented(),
    (&Method::POST, "/v1/completions") => not_implemented(),
    (&Method::POST, "/v1/embeddings") => not_implemented(),
    (&Method::POST, "/v1/rerank") => not_implemented(),
    _ => not_found(),
  }
}

async fn health(state: Arc<ProxyState>) -> ProxyResponse {
  // `len()` on both is a single read-lock acquisition each; cheap.
  // Counts will gain real meaning in Unit 2 (alphabetical /v1/models
  // listing) but the wire shape is locked here so clients can pin
  // against it from day one.
  let models_loaded = state.supervisors.len().await;
  let models_discovered = state.catalog.len().await;
  let body = json!({
    "status": "ok",
    "models_loaded": models_loaded,
    "models_discovered": models_discovered,
  });
  // serde_json::to_vec on a hand-built `Value` cannot fail.
  let bytes = serde_json::to_vec(&body).expect("json encoding of fixed shape");
  Ok(json_response(StatusCode::OK, bytes))
}

fn not_implemented() -> ProxyResponse {
  // OpenAI-shaped error body so clients see a recognisable payload
  // even on the 501 placeholder. Units 3/4 swap this for the real
  // handler — the wire shape they emit will be the same.
  let body = json!({
    "error": {
      "type": "not_implemented",
      "message": "endpoint not implemented yet",
    }
  });
  let bytes = serde_json::to_vec(&body).expect("json encoding of fixed shape");
  Ok(json_response(StatusCode::NOT_IMPLEMENTED, bytes))
}

fn not_found() -> ProxyResponse {
  let body = json!({
    "error": {
      "type": "not_found",
      "message": "no such route",
    }
  });
  let bytes = serde_json::to_vec(&body).expect("json encoding of fixed shape");
  Ok(json_response(StatusCode::NOT_FOUND, bytes))
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<BoxBody<Bytes, BodyError>> {
  let body = Full::new(Bytes::from(body)).boxed();
  Response::builder()
    .status(status)
    .header(hyper::header::CONTENT_TYPE, "application/json")
    .body(body)
    .expect("static headers always parse")
}

/// Construct an empty body — kept here for future handler arms that
/// need a no-content response without re-importing the util crate.
#[allow(dead_code)]
pub(crate) fn empty_body() -> BoxBody<Bytes, BodyError> {
  Empty::<Bytes>::new().boxed()
}
