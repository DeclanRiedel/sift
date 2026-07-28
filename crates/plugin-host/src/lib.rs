//! Server-internal extension package and process hosting.

pub mod framing;
pub mod lifecycle;
pub mod package;
pub mod registry;
pub mod supervisor;

pub use framing::*;
pub use lifecycle::*;
pub use package::*;
pub use registry::*;
pub use supervisor::*;
