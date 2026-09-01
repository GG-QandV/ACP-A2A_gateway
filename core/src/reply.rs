//! core/src/reply.rs
//!
//! The single "seam" of the architecture. All agent methods that
//! may become streaming in the future return Reply<T, U>, not T
//! directly. In Phase 1 only Complete is populated — Streaming exists
//! as an enum variant, but no agent returns it.
//!
//! Why this matters: when real streaming appears in Phase 2, only the
//! bodies of specific AcpAgent/A2aAgent impls change (they start returning
//! Reply::Streaming). Trait signatures, the dispatcher and the converter (convert.rs)
//! are NOT rewritten — they already handle both match variants today.

use tokio::sync::mpsc::UnboundedReceiver;

/// T — type of the final (non-streaming) reply.
/// U — type of an event-stream item (used only in Phase 2).
pub enum Reply<T, U> {
    /// The only variant actually returned in Phase 1.
    Complete(T),
    /// Appears in Phase 2. The place is already reserved in the signatures.
    Streaming(UnboundedReceiver<U>),
}
