use std::io;

use displaydoc::Display;
use thiserror::Error;

/// Errors raised at the NDJSON / agent-protocol boundary.
///
/// The variants cover the layers where loom-core is the only code that knows
/// about the wire (line framing, JSON parse, subprocess IO) plus the small set
/// of semantic outcomes a backend `LineParse` reports back upward.
#[derive(Debug, Display, Error)]
pub enum ProtocolError {
    /// invalid JSON on protocol line
    InvalidJson(#[from] serde_json::Error),

    /// unknown message type: {0}
    UnknownMessageType(String),

    /// io failure on agent stdio
    Io(#[from] io::Error),

    /// agent process exited with code {0}
    ProcessExit(i32),

    /// unexpected end of agent event stream
    UnexpectedEof,

    /// NDJSON line too long: {len} bytes (max {max})
    LineTooLong { len: usize, max: usize },

    /// operation not supported by this backend
    Unsupported,
}
