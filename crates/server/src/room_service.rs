//! Room-domain document operations shared by transport adapters.
//!
//! The positional snapshot-backed text model was removed in the protocol
//! reset. Loro-backed document mutation (durable update sequencing, validation,
//! and rebroadcast) lands here in G2/G3 as a per-document blocking actor, so
//! CRDT CPU work never runs on an axum request worker.
