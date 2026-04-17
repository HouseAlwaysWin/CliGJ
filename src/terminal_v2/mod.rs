//! Terminal v2 foundation (phase 1).
//!
//! This module is a clean-room foundation for the future high-performance
//! terminal view. It is intentionally decoupled from current Slint span-based
//! rendering, so we can migrate in phases.
#![allow(dead_code, unused_imports)]

pub mod model;
pub mod renderer;
pub mod session;

pub use model::{TerminalBuffer, TerminalCell, TerminalRow};
pub use renderer::{NoopRenderer, TerminalFrame, TerminalRenderer};
pub use session::TerminalSession;

