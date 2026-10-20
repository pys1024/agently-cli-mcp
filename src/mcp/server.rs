//! MCP server: shared [`AppState`] and the [`MailServer`] tool router.

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResponse;
use rmcp::{ErrorData, tool, tool_router};
use serde_json::{Map, Value};

use crate::cli_proc::CliOutcome;
use crate::cli_proc::runner::Runner;
use crate::config::Config;
use crate::confirm::ConfirmStore;
use crate::error::AppError;
use crate::guard::{AttachmentLimits, Sandbox};
use crate::search::SearchStore;
use crate::tools;

use super::result;

/// Shared state carried by every tool handler.
///
/// The `Arc` fields are process-wide and survive [`AppState::with_session`],
/// which swaps only `session_id` (used by the HTTP transport factory to give
/// each session its own confirmation/search identity).
pub struct AppState {
    pub cfg: Arc<Config>,
    pub runner: Arc<Runner>,
    pub sandbox: Arc<Sandbox>,
    pub confirm: Arc<ConfirmStore>,
    pub search: Arc<SearchStore>,
    pub session_id: String,
    pub limits: Arc<tokio::sync::RwLock<AttachmentLimits>>,
    pub watch_lock: Arc<tokio::sync::Mutex<()>>,
    seeded: Arc<tokio::sync::Mutex<bool>>,
}

impl AppState {
    /// Build shared state with default stores and limits.
    pub fn new(cfg: Arc<Config>, runner: Arc<Runner>, sandbox: Arc<Sandbox>) -> Self {
        Self {
            cfg,
            runner,
            sandbox,
            confirm: Arc::new(ConfirmStore::new()),
            search: Arc::new(SearchStore::new()),
            session_id: uuid::Uuid::new_v4().to_string(),
            limits: Arc::new(tokio::sync::RwLock::new(AttachmentLimits::default())),
            watch_lock: Arc::new(tokio::sync::Mutex::new(())),
            seeded: Arc::new(tokio::sync::Mutex::new(false)),
        }
    }

    /// Clone all shared `Arc`s and replace the session ID.
    pub fn with_session(&self, session_id: String) -> AppState {
        AppState {
            cfg: self.cfg.clone(),
            runner: self.runner.clone(),
            sandbox: self.sandbox.clone(),
            confirm: self.confirm.clone(),
            search: self.search.clone(),
            session_id,
            limits: self.limits.clone(),
            watch_lock: self.watch_lock.clone(),
            seeded: self.seeded.clone(),
        }
    }

    /// On the first account/list call, run `+me` and update the attachment
    /// limits from its constraints, falling back to the built-in defaults when
    /// `+me` fails or reports no constraints.
    pub async fn seed_limits(&self) -> Result<(), AppError> {
        {
            let seeded = self.seeded.lock().await;
            if *seeded {
                return Ok(());
            }
        }
        let mut seeded = self.seeded.lock().await;
        if *seeded {
            return Ok(());
        }

        let result = self
            .runner
            .run_json(
                &["+me".to_string()],
                Duration::from_secs(self.cfg.cli.timeout_secs),
            )
            .await;
        if let Ok(CliOutcome::Success(data)) = result
            && let Some(limits) = parse_limits(&data)
        {
            *self.limits.write().await = limits;
        }

        *seeded = true;
        Ok(())
    }
}

/// Parse attachment limits from a `+me` payload, tolerating the constraints at
/// `data.constraints.attachments` or directly under `data.constraints`.
fn parse_limits(data: &Value) -> Option<AttachmentLimits> {
    let constraints = data.get("constraints")?.as_object()?;
    let src: &Map<String, Value> = match constraints.get("attachments") {
        Some(Value::Object(inner)) => inner,
        _ => constraints,
    };

    let max_files = src.get("max_files").and_then(Value::as_u64)? as usize;
    let max_file_bytes = src
        .get("max_file_bytes")
        .and_then(Value::as_u64)
        .or_else(|| src.get("max_file_size").and_then(Value::as_u64))?;
    let max_total_bytes = src
        .get("max_total_bytes")
        .and_then(Value::as_u64)
        .or_else(|| src.get("max_total_size").and_then(Value::as_u64))?;

    Some(AttachmentLimits {
        max_files,
        max_file_bytes,
        max_total_bytes,
    })
}

/// The MCP server: an [`rmcp::ServerHandler`] wiring six read/account tools.
pub struct MailServer {
    pub state: Arc<AppState>,
}

/// Build a [`MailServer`] from shared state.
pub fn build_server(state: Arc<AppState>) -> MailServer {
    MailServer { state }
}

#[tool_router(server_handler)]
impl MailServer {
    #[tool(
        name = "mail_get_account",
        title = "Get current account and authentication status",
        description = "Return the active account (`+me`) and its authentication status.",
        annotations(read_only_hint = true)
    )]
    async fn mail_get_account(&self) -> Result<CallToolResponse, ErrorData> {
        match tools::account::get_account(&self.state).await {
            Ok((data, meta)) => Ok(result::ok("mail_get_account", data, meta).into()),
            Err(e) => Ok(result::err("mail_get_account", &e).into()),
        }
    }

    #[tool(
        name = "mail_refresh_auth",
        title = "Refresh authentication",
        description = "Refresh the stored OAuth credentials for the active account.",
        annotations(read_only_hint = false, idempotent_hint = true)
    )]
    async fn mail_refresh_auth(&self) -> Result<CallToolResponse, ErrorData> {
        match tools::account::refresh_auth(&self.state).await {
            Ok((data, meta)) => Ok(result::ok("mail_refresh_auth", data, meta).into()),
            Err(e) => Ok(result::err("mail_refresh_auth", &e).into()),
        }
    }

    #[tool(
        name = "mail_list_messages",
        title = "List messages",
        description = "List messages in a folder, optionally filtered by date, attachment presence, or read state.",
        annotations(read_only_hint = true)
    )]
    async fn mail_list_messages(
        &self,
        Parameters(args): Parameters<tools::read::ListArgs>,
    ) -> Result<CallToolResponse, ErrorData> {
        match tools::read::list_messages(&self.state, args).await {
            Ok((data, meta)) => Ok(result::ok("mail_list_messages", data, meta).into()),
            Err(e) => Ok(result::err("mail_list_messages", &e).into()),
        }
    }

    #[tool(
        name = "mail_get_message",
        title = "Read a message (marks it as read)",
        description = "Read a single message body. This marks the message as read.",
        annotations(read_only_hint = false, idempotent_hint = true)
    )]
    async fn mail_get_message(
        &self,
        Parameters(args): Parameters<tools::read::GetArgs>,
    ) -> Result<CallToolResponse, ErrorData> {
        match tools::read::get_message(&self.state, args).await {
            Ok((data, meta)) => Ok(result::ok("mail_get_message", data, meta).into()),
            Err(e) => Ok(result::err("mail_get_message", &e).into()),
        }
    }

    #[tool(
        name = "mail_search_messages",
        title = "Search messages",
        description = "Search messages. Cursors remember the filters that produced them, so a follow-up carrying only a cursor restores the full query.",
        annotations(read_only_hint = true)
    )]
    async fn mail_search_messages(
        &self,
        Parameters(args): Parameters<tools::read::SearchArgs>,
    ) -> Result<CallToolResponse, ErrorData> {
        match tools::read::search_messages(&self.state, args).await {
            Ok((data, meta)) => Ok(result::ok("mail_search_messages", data, meta).into()),
            Err(e) => Ok(result::err("mail_search_messages", &e).into()),
        }
    }

    #[tool(
        name = "mail_download_attachment",
        title = "Download an attachment",
        description = "Download an attachment to a directory inside the working directory.",
        annotations(read_only_hint = false)
    )]
    async fn mail_download_attachment(
        &self,
        Parameters(args): Parameters<tools::attachments::DownloadArgs>,
    ) -> Result<CallToolResponse, ErrorData> {
        match tools::attachments::download_attachment(&self.state, args).await {
            Ok((data, meta)) => Ok(result::ok("mail_download_attachment", data, meta).into()),
            Err(e) => Ok(result::err("mail_download_attachment", &e).into()),
        }
    }
}
