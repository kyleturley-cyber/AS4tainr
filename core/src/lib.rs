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
pub mod autl;
pub mod catalog;
pub mod cl;
pub mod cllist;
pub mod clpgm;
pub mod cmd;
pub mod cmdlist;
pub mod cmppfm;
pub mod cmtctl;
pub mod codepages;
pub mod collate;
pub mod commands;
pub mod cpyf;
pub mod dbr;
pub mod ddl;
pub mod dds;
pub mod ddslist;
pub mod ddsref;
pub mod dspf;
pub mod dspjob;
pub mod dspmod;
pub mod dtaara;
pub mod dtaq;
pub mod envvar;
pub mod fdout;
pub mod hostcmd;
pub mod hostdb;
pub mod hostsrv;
pub mod ibmcmds;
pub mod ifs;
pub mod ifs_commands;
pub mod ile;
pub mod impf;
pub mod import;
pub mod jobattr;
pub mod jobd;
pub mod joblog;
pub mod jobq;
pub mod jobs;
pub mod jobscde;
pub mod journal;
pub mod mapepire;
pub mod mapepire_ws;
pub mod mbrdesc;
pub mod menus;
pub mod msgf;
pub mod objdesc;
pub mod objlock;
pub mod outfile;
pub mod outq;
pub mod ovr;
pub mod pase_attr;
pub mod pase_system;
pub mod pdm;
pub mod pfmview;
pub mod pftrg;
pub mod pgmmsg;
pub mod pgmref;
pub mod prtf;
pub mod qsh;
pub mod qshdb2;
pub mod qshfs;
pub mod qsysinc;
pub mod qsyslib;
pub mod qtemp;
pub mod report;
pub mod rexx;
pub mod rla;
pub mod rpg;
pub mod rpyl;
pub mod rtvclsrc;
pub mod runtime;
pub mod save;
pub mod services;
pub mod seu;
pub mod source;
pub mod spool;
pub mod spoolapi;
pub mod sql;
pub mod sqlcat;
pub mod sqlfn;
pub mod sqljson;
pub mod sqljsonfn;
pub mod sqlmeta;
pub mod sqlpl;
pub mod sqlstmlist;
pub mod sqlvar;
pub mod sshd;
pub mod sysapi;
pub mod temporal;
pub mod tn5250;
pub mod users;
pub mod usrspc;
pub mod web;
pub mod workstn;
