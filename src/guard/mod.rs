//! Guard layer: enforce the path sandbox and redact secrets before they can
//! reach logs or error messages.

pub mod paths;
pub mod redact;

pub use paths::{AttachmentLimits, Sandbox};
