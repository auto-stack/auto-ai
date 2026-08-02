//! Client errors.
//!
//! The client is now a thin daemon HTTP client, so its error surface is small:
//! the daemon isn't reachable, the HTTP call failed, or the daemon returned an
//! error response.

/// Unified error type for the (now thin) daemon client.
#[derive(Debug)]
pub enum ClientError {
    /// The daemon could not be discovered or started.
    DaemonUnavailable,
    /// The HTTP request to the daemon failed (network/transport).
    Http(String),
    /// The daemon returned a non-success response or the body failed to parse.
    Api(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::DaemonUnavailable => write!(f, "daemon unavailable"),
            ClientError::Http(e) => write!(f, "HTTP error: {e}"),
            ClientError::Api(e) => write!(f, "API error: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.to_string())
    }
}
