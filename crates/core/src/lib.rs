//! Core logic of the dj-music-suite: convert, import, reorg and dedup.
//!
//! The crate is UI-agnostic: operations report progress through an [`Event`]
//! sink instead of printing, and return structured results instead of exit
//! codes. The CLI (and later the web server) maps these onto terminals,
//! browsers or exit codes.

pub mod config;
pub mod convert;
pub mod dedup;
pub mod import;
pub mod quality;
pub mod reorg;
pub mod scan;
pub mod tags;
pub mod template;

/// Errors that map onto distinct CLI exit codes: usage problems exit 2,
/// runtime problems exit 1.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Progress / output events emitted by core operations. The terminal
/// implementation draws indicatif bars; the web server forwards them over SSE.
#[derive(Debug, Clone)]
pub enum Event {
    /// a new progress phase begins with `total` items (any previous phase ends)
    Start(u64),
    /// one item of the current phase finished; the string names it
    Step(String),
    /// an informational output line (terminal: stdout)
    Line(String),
    /// a warning (terminal: stderr)
    Warn(String),
}

/// Thread-safe event sink: `&event` keeps large payloads from being cloned
/// into every call.
pub type Sink<'a> = &'a (dyn Fn(&Event) + Send + Sync);

fn usage(msg: impl Into<String>) -> Error {
    Error::Usage(msg.into())
}
