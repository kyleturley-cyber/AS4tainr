//! OpenEye core library: an IBM i-compatible runtime for containers.
//!
//! Modules:
//! - [`dds`]: parse DDS source for physical files
//! - [`catalog`]: libraries and file descriptions stored in PostgreSQL
//! - [`rla`]: record-level access (SETLL, CHAIN, READ, ...) over PostgreSQL
//! - [`bridge`]: the JSON-lines protocol the JVM (Jariko) uses for file I/O

pub mod bridge;
pub mod catalog;
pub mod dds;
pub mod rla;
