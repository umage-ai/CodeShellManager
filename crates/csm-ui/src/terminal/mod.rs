//! The terminal widget: a GPU-friendly renderer over `csm-term` (one textured mesh per view,
//! rebuilt only when the emulator or the view changed), input encoding, and the sources a pane
//! can show. Moved here from the Phase 0 spike.

pub mod atlas;
pub mod boxdraw;
pub mod claude_copy;
pub mod fonts;
pub mod local;
pub mod render;
pub mod resize;
pub mod selection;
pub mod source;
pub mod view;

pub use local::{LocalTerminal, Repaint, TermStatus};
pub use render::TermStyle;
pub use source::{SourceState, StaticTerminal, TerminalSource, lock};
pub use view::{FrameCounters, TerminalRenderer, TerminalView, ViewOptions, ViewResponse};
