//! as4tainr core library: an IBM i-compatible runtime for containers.
//!
//! Modules:
//! - [`dds`]: parse DDS source for physical files
//! - [`catalog`]: libraries and file descriptions stored in PostgreSQL
//! - [`rla`]: record-level access (SETLL, CHAIN, READ, ...) over PostgreSQL
//! - [`runtime`]: what a running program shares with its job (database session, display)
//! - [`sql`]: SQL in the DB2 for i dialect
//! - [`dspf`], [`workstn`]: display files, subfiles and the screen
//! - [`dtaara`]: data areas
//! - [`authority`]: object owners and authorities (*ALL, *CHANGE, *USE, *EXCLUDE)
//! - [`spool`]: spooled files (printed output)
//! - [`import`]: `as4tainr import`: an application's source and data into a library
//! - [`source`]: source physical files and members (QRPGLESRC...)
//! - [`menus`]: menus (GO, CRTMNU) and the system menus
//! - [`jobs`]: jobs, job queues, job logs, message queues, the QBATCH subsystem
//! - [`web`]: the browser 5250 terminal

pub mod authority;
pub mod catalog;
pub mod cl;
pub mod clpgm;
pub mod commands;
pub mod dds;
pub mod dspf;
pub mod dtaara;
pub mod import;
pub mod jobs;
pub mod menus;
pub mod rla;
pub mod rpg;
pub mod runtime;
pub mod source;
pub mod spool;
pub mod sql;
pub mod tn5250;
pub mod users;
pub mod web;
pub mod workstn;
