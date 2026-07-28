//! Pure wire contracts shared by extension authors and the Sift host.
//!
//! This crate deliberately contains no process, filesystem, networking, or
//! asynchronous runtime code.

pub mod connection;
pub mod driver;
pub mod identity;
pub mod manifest;
pub mod operation;
pub mod rpc;

pub use connection::*;
pub use driver::*;
pub use identity::*;
pub use manifest::*;
pub use operation::*;
pub use rpc::*;

/// Extension process-envelope protocol version.
pub const EXTENSION_RPC_VERSION: u32 = 1;
/// Database-driver method-family protocol version.
pub const DRIVER_RPC_VERSION: u32 = 1;
