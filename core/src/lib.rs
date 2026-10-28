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
pub mod cllist;
pub mod clpgm;
pub mod cmd;
pub mod cmdlist;
pub mod cmtctl;
pub mod collate;
pub mod commands;
pub mod cpyf;
pub mod dbr;
pub mod ddl;
pub mod dds;
pub mod ddslist;
pub mod dspf;
pub mod dspjob;
pub mod dtaara;
pub mod dtaq;
pub mod envvar;
pub mod fdout;
pub mod ibmcmds;
pub mod ifs;
pub mod ifs_commands;
pub mod ile;
pub mod import;
pub mod jobd;
pub mod joblog;
pub mod jobs;
pub mod journal;
pub mod menus;
pub mod msgf;
pub mod objdesc;
pub mod outfile;
pub mod outq;
pub mod ovr;
pub mod pase_system;
pub mod pdm;
pub mod pfmview;
pub mod pftrg;
pub mod pgmmsg;
pub mod prtf;
pub mod qsh;
pub mod qshfs;
pub mod qsysinc;
pub mod qtemp;
pub mod report;
pub mod rexx;
pub mod rla;
pub mod rpg;
pub mod runtime;
pub mod save;
pub mod services;
pub mod source;
pub mod spool;
pub mod spoolapi;
pub mod sql;
pub mod sqlcat;
pub mod sqlfn;
pub mod sqljson;
pub mod sqljsonfn;
pub mod sqlpl;
pub mod sqlstmlist;
pub mod sysapi;
pub mod temporal;
pub mod tn5250;
pub mod users;
pub mod usrspc;
pub mod web;
pub mod workstn;
