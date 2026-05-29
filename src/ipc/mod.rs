//! Inter-process communication between llamastash frontends (TUI, CLI) and
//! the daemon. JSON-RPC 2.0 over HTTP loopback with bearer-token auth.
//!
//! Module layout:
//! - `protocol` — JSON-RPC types (`Request`, `Response`, `ErrorObject`).
//! - `methods` — server-side method dispatch.
//! - `client` — async client used by the TUI and CLI; talks the HTTP
//!   control plane defined in [`crate::daemon::control_plane`].

pub mod client;
pub mod methods;
pub mod protocol;

#[allow(unused_imports)]
pub use client::{Client, ClientError, DEFAULT_CALL_TIMEOUT};
#[allow(unused_imports)]
pub use methods::{dispatch_request, MethodContext};
#[allow(unused_imports)]
pub use protocol::{ErrorCode, ErrorObject, Request, Response, JSONRPC_VERSION};
