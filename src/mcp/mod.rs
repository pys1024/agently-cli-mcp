//! MCP transport and server glue.

pub mod result;
pub mod server;
pub mod stdio;

pub use server::{AppState, MailServer, build_server};
