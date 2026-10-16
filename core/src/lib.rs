//! ibmish core library: an IBM i-compatible runtime for containers.
//!
//! Modules:
//! - [`dds`]: parse DDS source for physical files
//! - [`catalog`]: libraries and file descriptions stored in PostgreSQL
//! - [`rla`]: record-level access (SETLL, CHAIN, READ, ...) over PostgreSQL
//! - [`bridge`]: the JSON-lines protocol the JVM (Jariko) uses for file I/O
//! - [`sql`]: SQL in the DB2 for i dialect
//! - [`dspf`], [`workstn`]: display files, subfiles and the screen

pub mod bridge;
pub mod catalog;
pub mod cl;
pub mod commands;
pub mod dds;
pub mod dspf;
pub mod rla;
pub mod sql;
pub mod tn5250;
pub mod users;
pub mod workstn;
