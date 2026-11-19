//! `AlertEvent` and `AlertType` (C# `Models/AlertEvent.cs`). Never persisted.

/// What kind of attention a session wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertType {
    InputRequired,
    ToolApproval,
    Question,
    Idle,
}

/// An alert raised by [`crate::alert_detector::AlertDetector`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertEvent {
    pub session_id: String,
    pub session_name: String,
    pub message: String,
    pub alert_type: AlertType,
    /// Milliseconds since the Unix epoch, UTC, as supplied by the caller's clock.
    pub timestamp_ms: i64,
}
