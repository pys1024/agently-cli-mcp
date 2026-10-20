//! Confirmation state: pending-confirmation cache and deterministic summary.
//!
//! Write tools run the CLI twice: the first call yields a one-time `ctk_`
//! token bound to the exact request, the second (after human confirmation)
//! replays the cached canonical arguments with that token. The [`store`]
//! module keeps the pending record between the two calls; the [`summary`]
//! module renders the human-facing confirmation prompt from the validated
//! tool inputs, never from the CLI's lossy merged summary.

pub mod store;
pub mod summary;

pub use store::{ConfirmStore, Pending};
pub use summary::{AttachmentMeta, SourceMessage, SummaryAction, SummaryInput};
