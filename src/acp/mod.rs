//! ACP (Agent Client Protocol) client — talk to external coding agents over stdio.

mod connection;
mod map;
mod mcp;
mod session;

pub use connection::AcpConnection;
pub use map::ToolCallUpdateSummary;
pub use session::{SessionRef, SessionSummary};
