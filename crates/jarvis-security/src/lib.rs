//! JARVIS Security Policy Engine.
//!
//! Every tool call is described as an [`ActionRequest`] before it runs and
//! checked by [`SecurityPolicy::evaluate`]: allowed, refused, or held until
//! the user approves it through the [`ApprovalBroker`]. Decisions and
//! outcomes are written to the hash-chained [`AuditLog`].
//!
//! What this crate does **not** provide: process isolation. Tools run with
//! the user's rights; the policy limits what they are asked to do (scoped
//! paths, program allowlist, approvals), it does not sandbox them.

mod approvals;
mod audit;
mod policy;
mod scope;
mod types;

pub use approvals::{ApprovalBroker, ApprovalOutcome, ApprovalRequest, PendingApproval};
pub use audit::{AuditEntry, AuditEvent, AuditLog, AuditVerification, NewAuditEntry};
pub use policy::{PolicyConfig, SecurityPolicy, check_ip, check_url, is_plain_program_name};
pub use scope::{FileScope, ScopeError, is_sensitive};
pub use types::{ActionRequest, Capability, PolicyDecision, RiskLevel};
