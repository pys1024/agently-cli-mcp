//! Streamable HTTP transport: an axum server exposing `/mcp` (MCP over
//! Streamable HTTP) and `/healthz`, with optional bearer authentication and
//! Host-header allowlisting.

use std::sync::Arc;

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

    let session_manager = Arc::new(LocalSessionManager::default());

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

/// Resolve the bearer token from `http_token` or `http_token_file` (trimmed).
fn resolve_token(cfg: &HttpConfig) -> Result<Option<String>, AppError> {
    if let Some(token) = &cfg.http_token {
        return Ok(Some(token.clone()));
    }
    if let Some(path) = &cfg.http_token_file {
        let token = std::fs::read_to_string(path)
            .map_err(|e| AppError::Other(format!("failed to read HTTP token file `{path}`: {e}")))?
            .trim()
            .to_string();
        return Ok(Some(token));
    }
    Ok(None)
}

/// Bearer-token middleware: requires `Authorization: Bearer <token>` with a
/// constant-time comparison.
async fn require_bearer(State(token): State<String>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(token.as_bytes())));

    if authorized {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Liveness probe. Always public, never behind the bearer middleware.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Wait for Ctrl-C, SIGTERM (unix), or the shutdown token.
async fn shutdown_signal(shutdown: CancellationToken) {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
            _ = shutdown.cancelled_owned() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = shutdown.cancelled_owned() => {},
        }
    }
}
