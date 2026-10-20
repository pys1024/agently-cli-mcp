//! Streamable HTTP transport: an axum server exposing `/mcp` (MCP over
//! Streamable HTTP) and `/healthz`, with optional bearer authentication and
//! Host-header allowlisting.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::HttpConfig;
use crate::error::AppError;
use crate::mcp::{AppState, MailServer, build_server};

/// Serve the MCP server over Streamable HTTP until `shutdown` is cancelled.
pub async fn serve(
    state: Arc<AppState>,
    cfg: &HttpConfig,
    shutdown: CancellationToken,
) -> Result<(), AppError> {
    let token = resolve_token(cfg)?;

    if token.is_none() && !cfg.listen.ip().is_loopback() {
        eprintln!(
            "WARNING: serving HTTP on non-loopback address {} without an auth token; \
             any client that can reach this port can control your mail. \
             Set --http-token or --http-token-file to require bearer authentication.",
            cfg.listen
        );
    }

    let session_manager = Arc::new(session_manager(cfg.session_idle_secs));

    let factory = {
        let state = state.clone();
        move || -> Result<MailServer, std::io::Error> {
            let session_id = uuid::Uuid::new_v4().to_string();
            let session_state = state.with_session(session_id);
            Ok(build_server(Arc::new(session_state)))
        }
    };

    let mut server_config =
        StreamableHttpServerConfig::default().with_cancellation_token(shutdown.child_token());
    if cfg.disable_host_validation {
        server_config = server_config.disable_allowed_hosts();
    } else {
        server_config = server_config.with_allowed_hosts(cfg.allowed_hosts.clone());
    }
    if !cfg.cors_origins.is_empty() {
        // Origin validation is off by default; an explicit `--cors-origin` list
        // turns it on and restricts inbound `Origin` headers to those entries.
        server_config = server_config
            .with_allowed_origins(cfg.cors_origins.clone())
            .enforce_origin_validation();
    }

    let service = StreamableHttpService::new(factory, session_manager, server_config);

    let mcp_router = Router::new().nest_service("/mcp", service);
    let mcp_router = match token {
        Some(token) => mcp_router.layer(middleware::from_fn_with_state(token, require_bearer)),
        None => mcp_router,
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .merge(mcp_router);

    let listener = TcpListener::bind(cfg.listen)
        .await
        .map_err(|e| AppError::Other(format!("failed to bind {}: {e}", cfg.listen)))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .map_err(|e| AppError::Other(format!("HTTP server error: {e}")))?;

    Ok(())
}

/// Resolve the bearer token from `http_token` or `http_token_file`. A token
/// that is empty or only whitespace is treated as "no token configured", so the
/// endpoint stays open and the non-loopback warning still applies.
fn resolve_token(cfg: &HttpConfig) -> Result<Option<String>, AppError> {
    let raw = if let Some(token) = &cfg.http_token {
        Some(token.clone())
    } else if let Some(path) = &cfg.http_token_file {
        Some(
            std::fs::read_to_string(path)
                .map_err(|e| {
                    AppError::Other(format!("failed to read HTTP token file `{path}`: {e}"))
                })?
                .trim()
                .to_string(),
        )
    } else {
        None
    };

    Ok(raw.and_then(|token| {
        let token = token.trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }))
}

/// Build a [`LocalSessionManager`] whose idle timeout is `session_idle_secs`.
///
/// The session manager's `keep_alive` closes sessions that have been inactive
/// for that long, matching the `--session-idle-secs` / `session_idle_secs`
/// configuration.
fn session_manager(session_idle_secs: u64) -> LocalSessionManager {
    let mut manager = LocalSessionManager::default();
    manager.session_config.keep_alive = Some(Duration::from_secs(session_idle_secs));
    manager
}

/// Bearer-token middleware: requires `Authorization: Bearer <token>` with a
/// constant-time comparison. The scheme is matched case-insensitively and any
/// amount of whitespace is accepted between it and the credential; a failed
/// check answers `401` with a `WWW-Authenticate: Bearer` challenge.
async fn require_bearer(State(token): State<String>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(extract_bearer)
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(token.as_bytes())));

    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response()
    }
}

/// Extract the credential from an `Authorization: <scheme> <credential>` value,
/// accepting a case-insensitive `Bearer` scheme and any amount of whitespace.
fn extract_bearer(value: &str) -> Option<&str> {
    let value = value.trim();
    let split_at = value.find(char::is_whitespace)?;
    let (scheme, rest) = value.split_at(split_at);
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let credential = rest.trim_start();
    if credential.is_empty() {
        None
    } else {
        Some(credential)
    }
}

/// Liveness probe. Always public, never behind the bearer middleware.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Wait for Ctrl-C, SIGTERM (unix), or the shutdown token, then cancel the
/// token so rmcp closes active sessions/SSE streams before the graceful
/// shutdown drain completes. Cancelling is idempotent, so the token path is a
/// no-op (the token is already cancelled there).
async fn shutdown_signal(shutdown: CancellationToken) {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
            _ = shutdown.cancelled() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = shutdown.cancelled() => {},
        }
    }
    shutdown.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manager_sets_configured_keep_alive() {
        let manager = session_manager(42);
        assert_eq!(
            manager.session_config.keep_alive,
            Some(Duration::from_secs(42))
        );
    }

    #[test]
    fn extract_bearer_is_case_insensitive_and_whitespace_agnostic() {
        assert_eq!(extract_bearer("Bearer abc"), Some("abc"));
        assert_eq!(extract_bearer("bearer abc"), Some("abc"));
        assert_eq!(extract_bearer("BEARER   abc"), Some("abc"));
        assert_eq!(extract_bearer("  bearer \t abc  "), Some("abc"));
    }

    #[test]
    fn extract_bearer_rejects_other_schemes_and_missing_credentials() {
        assert_eq!(extract_bearer("Basic abc"), None);
        assert_eq!(extract_bearer("Bearer"), None);
        assert_eq!(extract_bearer("Bearer   "), None);
        assert_eq!(extract_bearer(""), None);
        assert_eq!(extract_bearer("Bearerish abc"), None);
    }
}
