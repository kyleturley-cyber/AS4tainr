//! TN5250 display server: 5250 green-screen sessions over telnet
//! (RFC 1205, RFC 2877, RFC 4777).

pub mod ds;
pub mod ebcdic;
pub mod session;
pub mod telnet;

pub use session::{serve, ServerConfig};
