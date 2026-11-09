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
/// Oldest supported database-driver method-family protocol version.
pub const DRIVER_RPC_VERSION: u32 = 1;

/// Highest supported Driver method family; v1 remains supported.
pub const DRIVER_RPC_LATEST_VERSION: u32 = 4;
/// Select the highest version implemented by the host and admitted by both peers.
pub fn negotiate_driver_rpc(manifest: VersionRange, hello: VersionRange) -> Option<u32> {
    if !manifest.is_valid() || !hello.is_valid() {
        return None;
    }
    (DRIVER_RPC_VERSION..=DRIVER_RPC_LATEST_VERSION)
        .rev()
        .find(|version| manifest.contains(*version) && hello.contains(*version))
}
