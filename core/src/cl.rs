//! CL command parsing, driven by a table of command definitions.
//!
//! Syntax (from the public CL reference): a command name followed by
//! parameters, each either positional or `KEYWORD(value ...)`. Values are
//! names (folded to upper case), special values like `*LIBL`, qualified
//! names `LIB/OBJ`, numbers, or quoted strings `'it''s'` (case kept).
//!
//! Each command is declared once, as data ([`CommandDef`]); the same table
//! drives parsing and prompting (F4): [`parse_partial`] reads what was typed
//! so far, [`build`] turns the prompt's values back into a command.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An object name (1-10 chars) or a special value starting with '*'.
    Name,
    /// Free text, usually quoted.
    Text,
    /// A list of values (e.g. CALL PARM).
    List,
    /// A qualified object name `LIB/OBJ` (library defaults to *LIBL); the
    /// object part may be generic (`CUST*`) or *ALL where the command allows.
    Qualified,
    /// A password: prompted in a nondisplay field.
    Password,
    /// A whole command as a value (SBMJOB CMD): taken as written, nested
    /// parentheses and all.
    Command,
}

#[derive(Debug, Clone, Copy)]
pub struct ParamDef {
    pub keyword: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// Can be given without its keyword, in table order.
    pub positional: bool,
    pub default: Option<&'static str>,
    pub prompt: &'static str,
    /// Allowed values shown next to the prompt field ("Name, *LIBL").
    pub choices: &'static str,
}

impl ParamDef {
    const fn ch(mut self, choices: &'static str) -> Self {
        self.choices = choices;
        self
    }

    const fn keyword_only(mut self) -> Self {
        self.positional = false;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamDef],
}

const fn p(
    keyword: &'static str,
    kind: Kind,
    required: bool,
    default: Option<&'static str>,
    prompt: &'static str,
) -> ParamDef {
    ParamDef {
        keyword,
        kind,
        required,
        positional: true,
        default,
        prompt,
        choices: "",
    }
}

pub static COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "CRTLIB",
        description: "Create Library",
        params: &[
            p("LIB", Kind::Name, true, None, "Library").ch("Name"),
            p("TYPE", Kind::Name, false, Some("*PROD"), "Library type").ch("*PROD, *TEST"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'"),
        ],
    },
    CommandDef {
        name: "DSPLIB",
        description: "Display Library",
        params: &[p("LIB", Kind::Name, false, Some("*LIBL"), "Library").ch("Name, *LIBL, *CURLIB")],
    },
    CommandDef {
        name: "DSPLIBL",
        description: "Display Library List",
        params: &[],
    },
    CommandDef {
        name: "ADDLIBLE",
        description: "Add Library List Entry",
        params: &[
            p("LIB", Kind::Name, true, None, "Library").ch("Name"),
            p(
                "POSITION",
                Kind::Name,
                false,
                Some("*FIRST"),
                "Library list position",
            )
            .ch("*FIRST, *LAST"),
        ],
    },
    CommandDef {
        name: "RMVLIBLE",
        description: "Remove Library List Entry",
        params: &[p("LIB", Kind::Name, true, None, "Library").ch("Name")],
    },
    CommandDef {
        name: "CHGCURLIB",
        description: "Change Current Library",
        params: &[p("CURLIB", Kind::Name, true, None, "Current library").ch("Name, *CRTDFT")],
    },
    CommandDef {
        name: "CHGLIBL",
        description: "Change Library List",
        params: &[
            p("LIBL", Kind::List, true, None, "Libraries for current job").ch("Name, *SAME, *NONE"),
            p(
                "CURLIB",
                Kind::Name,
                false,
                Some("*SAME"),
                "Current library",
            )
            .ch("Name, *SAME, *CRTDFT"),
        ],
    },
    CommandDef {
        name: "RUNSQL",
        description: "Run SQL Statement",
        params: &[
            p("SQL", Kind::Text, true, None, "SQL statement"),
            p(
                "COMMIT",
                Kind::Name,
                false,
                Some("*NONE"),
                "Commitment control",
            )
            .ch("*NONE")
            .keyword_only(),
            p("NAMING", Kind::Name, false, Some("*SYS"), "Naming")
                .ch("*SYS, *SQL")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "RUNSQLSTM",
        description: "Run SQL Statements",
        params: &[
            p("SRCFILE", Kind::Qualified, false, None, "Source file").ch("Name"),
            p("SRCMBR", Kind::Name, false, None, "Source member").ch("Name"),
            p("SRCSTMF", Kind::Text, false, None, "Source stream file").ch("Path name"),
            p(
                "COMMIT",
                Kind::Name,
                false,
                Some("*CHG"),
                "Commitment control",
            )
            .ch("*CHG, *ALL, *CS, *NONE, *NC, *RR")
            .keyword_only(),
            p("NAMING", Kind::Name, false, Some("*SYS"), "Naming")
                .ch("*SYS, *SQL")
                .keyword_only(),
            p("ERRLVL", Kind::Text, false, Some("10"), "Severity level")
                .ch("0-40")
                .keyword_only(),
            p(
                "DFTRDBCOL",
                Kind::Name,
                false,
                Some("*NONE"),
                "Default collection",
            )
            .ch("Name, *NONE")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "STRSQL",
        description: "Start SQL Interactive Session",
        params: &[],
    },
    CommandDef {
        name: "CALL",
        description: "Call Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p("PARM", Kind::List, false, None, "Parameters"),
        ],
    },
    CommandDef {
        name: "SIGNOFF",
        description: "Sign Off",
        params: &[p("LOG", Kind::Name, false, Some("*NOLIST"), "Job log").ch("*NOLIST, *LIST")],
    },
    CommandDef {
        name: "CRTUSRPRF",
        description: "Create User Profile",
        params: &[
            p("USRPRF", Kind::Name, true, None, "User profile").ch("Name"),
            p(
                "PASSWORD",
                Kind::Password,
                false,
                Some("*USRPRF"),
                "User password",
            )
            .ch("Name, *USRPRF, *NONE"),
            p(
                "PWDEXP",
                Kind::Name,
                false,
                Some("*NO"),
                "Set password to expired",
            )
            .ch("*NO, *YES")
            .keyword_only(),
            p("STATUS", Kind::Name, false, Some("*ENABLED"), "Status")
                .ch("*ENABLED, *DISABLED")
                .keyword_only(),
            p("USRCLS", Kind::Name, false, Some("*USER"), "User class")
                .ch("*USER, *SYSOPR, *PGMR...")
                .keyword_only(),
            p(
                "CURLIB",
                Kind::Name,
                false,
                Some("*CRTDFT"),
                "Current library",
            )
            .ch("Name, *CRTDFT")
            .keyword_only(),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'").keyword_only(),
            p(
                "SPCAUT",
                Kind::List,
                false,
                Some("*USRCLS"),
                "Special authority",
            )
            .ch("*USRCLS, *NONE...")
            .keyword_only(),
            p(
                "INLPGM",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Initial program to call",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "INLMNU",
                Kind::Qualified,
                false,
                Some("MAIN"),
                "Initial menu",
            )
            .ch("Name, MAIN, *SIGNOFF")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CHGUSRPRF",
        description: "Change User Profile",
        params: &[
            p("USRPRF", Kind::Name, true, None, "User profile").ch("Name"),
            p(
                "PWDEXP",
                Kind::Name,
                false,
                Some("*SAME"),
                "Set password to expired",
            )
            .ch("*SAME, *NO, *YES")
            .keyword_only(),
            p("STATUS", Kind::Name, false, Some("*SAME"), "Status")
                .ch("*SAME, *ENABLED, *DISABLED")
                .keyword_only(),
            p(
                "CURLIB",
                Kind::Name,
                false,
                Some("*SAME"),
                "Current library",
            )
            .ch("Name, *SAME, *CRTDFT")
            .keyword_only(),
            p(
                "INLPGM",
                Kind::Qualified,
                false,
                Some("*SAME"),
                "Initial program to call",
            )
            .ch("Name, *SAME, *NONE")
            .keyword_only(),
            p(
                "INLMNU",
                Kind::Qualified,
                false,
                Some("*SAME"),
                "Initial menu",
            )
            .ch("Name, *SAME, MAIN, *SIGNOFF")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SAME"),
                "Text 'description'",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "GO",
        description: "Go to Menu",
        params: &[p("MENU", Kind::Qualified, true, None, "Menu").ch("Name, MAIN")],
    },
    CommandDef {
        name: "CRTMNU",
        description: "Create Menu",
        params: &[
            p("MENU", Kind::Qualified, true, None, "Menu").ch("Name"),
            p("TYPE", Kind::Name, false, Some("*DSPF"), "Menu type").ch("*DSPF, *PGM, *UIM"),
            p("PGM", Kind::Qualified, false, Some("*MENU"), "Program").ch("Name, *MENU"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'").keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTCMD",
        description: "Create Command",
        params: &[
            p("CMD", Kind::Qualified, true, None, "Command").ch("Name"),
            p(
                "PGM",
                Kind::Qualified,
                true,
                None,
                "Program to process command",
            )
            .ch("Name, *REXX"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QCMDSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*CMD"), "Source member").ch("Name, *CMD"),
            p(
                "REXSRCFILE",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "REXX source file",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "REXSRCMBR",
                Kind::Name,
                false,
                Some("*NONE"),
                "REXX source member",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "REXCMDENV",
                Kind::Name,
                false,
                Some("*COMMAND"),
                "REXX command environment",
            )
            .keyword_only(),
            p(
                "REXEXITPGM",
                Kind::List,
                false,
                Some("*NONE"),
                "REXX exit programs",
            )
            .keyword_only(),
            p("THDSAFE", Kind::Name, false, Some("*NO"), "Threadsafe")
                .ch("*NO, *YES, *COND")
                .keyword_only(),
            p(
                "MLTTHDACN",
                Kind::Name,
                false,
                Some("*SYSVAL"),
                "Multithreaded job action",
            )
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "VLDCKR",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Validity checking program",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "MODE",
                Kind::List,
                false,
                Some("*ALL"),
                "Mode in which valid",
            )
            .keyword_only(),
            p(
                "ALLOW",
                Kind::List,
                false,
                Some("*ALL"),
                "Where allowed to run",
            )
            .keyword_only(),
            p(
                "ALWLMTUSR",
                Kind::Name,
                false,
                Some("*NO"),
                "Allow limited users",
            )
            .keyword_only(),
            p(
                "MAXPOS",
                Kind::Text,
                false,
                Some("*NOMAX"),
                "Maximum positional parameters",
            )
            .keyword_only(),
            p(
                "PMTFILE",
                Kind::List,
                false,
                Some("*NONE"),
                "Message file for prompt text",
            )
            .keyword_only(),
            p(
                "MSGF",
                Kind::Qualified,
                false,
                Some("QCPFMSG"),
                "Message file",
            )
            .keyword_only(),
            p(
                "HLPSHELF",
                Kind::Name,
                false,
                Some("*NONE"),
                "Help bookshelf",
            )
            .keyword_only(),
            p(
                "HLPPNLGRP",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Help panel group",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p("HLPID", Kind::Name, false, Some("*NONE"), "Help identifier").keyword_only(),
            p(
                "HLPSCHIDX",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Help search index",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "CURLIB",
                Kind::Name,
                false,
                Some("*NOCHG"),
                "Current library",
            )
            .keyword_only(),
            p(
                "PRDLIB",
                Kind::Name,
                false,
                Some("*NOCHG"),
                "Product library",
            )
            .keyword_only(),
            p(
                "PMTOVRPGM",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Prompt override program",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p("AUT", Kind::Name, false, Some("*LIBCRTAUT"), "Authority").keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace command",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p("ENBGUI", Kind::Name, false, Some("*NO"), "Enable GUI").keyword_only(),
        ],
    },
    CommandDef {
        name: "DLTCMD",
        description: "Delete Command",
        params: &[p("CMD", Kind::Qualified, true, None, "Command").ch("Name")],
    },
    CommandDef {
        name: "DSPCMD",
        description: "Display Command",
        params: &[p("CMD", Kind::Qualified, true, None, "Command").ch("Name")],
    },
    CommandDef {
        name: "STRCMTCTL",
        description: "Start Commitment Control",
        params: &[
            p("LCKLVL", Kind::Name, true, None, "Lock level").ch("*CHG, *CS, *ALL"),
            p(
                "CMTSCOPE",
                Kind::Name,
                false,
                Some("*ACTGRP"),
                "Commitment definition scope",
            )
            .ch("*ACTGRP, *JOB"),
            p("NFYOBJ", Kind::List, false, Some("*NONE"), "Notify object").keyword_only(),
            p(
                "DFTJRN",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Default journal",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "OMTJRNE",
                Kind::Name,
                false,
                Some("*NONE"),
                "Omit journal entry",
            )
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*DFT"),
                "Text 'description'",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "ENDCMTCTL",
        description: "End Commitment Control",
        params: &[],
    },
    CommandDef {
        name: "COMMIT",
        description: "Commit",
        params: &[p(
            "CMTID",
            Kind::Text,
            false,
            Some("*NONE"),
            "Commit identification",
        )],
    },
    CommandDef {
        name: "ROLLBACK",
        description: "Rollback",
        params: &[],
    },
    CommandDef {
        name: "OVRDBF",
        description: "Override with Data Base File",
        params: &[
            p("FILE", Kind::Name, true, None, "File being overridden").ch("Name"),
            p(
                "TOFILE",
                Kind::Qualified,
                false,
                Some("*FILE"),
                "Overriding to data base file",
            )
            .ch("Name, *FILE"),
            p(
                "MBR",
                Kind::Name,
                false,
                Some("*FIRST"),
                "Overriding member",
            )
            .ch("Name, *FIRST, *LAST, *ALL"),
            p(
                "POSITION",
                Kind::List,
                false,
                Some("*NONE"),
                "Starting position in file",
            )
            .ch("*NONE, *START, *END, *RRN, *KEYB..."),
            p(
                "RCDFMTLCK",
                Kind::List,
                false,
                Some("*NONE"),
                "Record format lock",
            )
            .keyword_only(),
            p(
                "FRCRATIO",
                Kind::Text,
                false,
                Some("*NONE"),
                "Records retrieved at once",
            )
            .keyword_only(),
            p(
                "WAITFILE",
                Kind::Text,
                false,
                Some("*IMMED"),
                "Maximum file wait time",
            )
            .keyword_only(),
            p(
                "WAITRCD",
                Kind::Text,
                false,
                Some("*NOMAX"),
                "Maximum record wait time",
            )
            .keyword_only(),
            p(
                "NBRRCDS",
                Kind::Text,
                false,
                Some("*NONE"),
                "Records retrieved at once",
            )
            .keyword_only(),
            p(
                "EOFDLY",
                Kind::Text,
                false,
                Some("*NONE"),
                "EOF retry delay in seconds",
            )
            .keyword_only(),
            p(
                "LVLCHK",
                Kind::Name,
                false,
                Some("*NONE"),
                "Record format level check",
            )
            .keyword_only(),
            p(
                "EXPCHK",
                Kind::Name,
                false,
                Some("*NONE"),
                "Expiration date check",
            )
            .keyword_only(),
            p("INHWRT", Kind::Name, false, Some("*NONE"), "Inhibit write").keyword_only(),
            p(
                "SECURE",
                Kind::Name,
                false,
                Some("*NO"),
                "Secure from other overrides",
            )
            .keyword_only(),
            p(
                "OVRSCOPE",
                Kind::Name,
                false,
                Some("*ACTGRPDFN"),
                "Override scope",
            )
            .ch("*ACTGRPDFN, *CALLLVL, *JOB")
            .keyword_only(),
            p(
                "SHARE",
                Kind::Name,
                false,
                Some("*NO"),
                "Share open data path",
            )
            .keyword_only(),
            p("OPNSCOPE", Kind::Name, false, Some("*NONE"), "Open scope").keyword_only(),
            p(
                "SEQONLY",
                Kind::List,
                false,
                Some("*NONE"),
                "Limit to sequential only",
            )
            .keyword_only(),
            p(
                "DSTDTA",
                Kind::Name,
                false,
                Some("*NONE"),
                "Distribute data",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "OVRPRTF",
        description: "Override with Printer File",
        params: &[
            p("FILE", Kind::Name, true, None, "File being overridden").ch("Name, *PRTF"),
            p(
                "TOFILE",
                Kind::Qualified,
                false,
                Some("*FILE"),
                "Overriding to printer file",
            )
            .ch("Name, *FILE"),
            p("DEV", Kind::Name, false, Some("*NONE"), "Device").keyword_only(),
            p("PAGESIZE", Kind::List, false, Some("*NONE"), "Page size")
                .ch("length width")
                .keyword_only(),
            p("LPI", Kind::Text, false, Some("*NONE"), "Lines per inch").keyword_only(),
            p(
                "CPI",
                Kind::Text,
                false,
                Some("*NONE"),
                "Characters per inch",
            )
            .keyword_only(),
            p(
                "OVRFLW",
                Kind::Text,
                false,
                Some("*NONE"),
                "Overflow line number",
            )
            .keyword_only(),
            p(
                "OUTQ",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Output queue",
            )
            .keyword_only(),
            p("FORMTYPE", Kind::List, false, Some("*NONE"), "Form type").keyword_only(),
            p("COPIES", Kind::Text, false, Some("*NONE"), "Copies").keyword_only(),
            p(
                "HOLD",
                Kind::Name,
                false,
                Some("*NONE"),
                "Hold spooled file",
            )
            .keyword_only(),
            p(
                "SAVE",
                Kind::Name,
                false,
                Some("*NONE"),
                "Save spooled file",
            )
            .keyword_only(),
            p(
                "SPLFNAME",
                Kind::Name,
                false,
                Some("*NONE"),
                "Spooled file name",
            )
            .keyword_only(),
            p("USRDTA", Kind::Text, false, Some("*NONE"), "User data").keyword_only(),
            p(
                "SECURE",
                Kind::Name,
                false,
                Some("*NO"),
                "Secure from other overrides",
            )
            .keyword_only(),
            p(
                "OVRSCOPE",
                Kind::Name,
                false,
                Some("*ACTGRPDFN"),
                "Override scope",
            )
            .ch("*ACTGRPDFN, *CALLLVL, *JOB")
            .keyword_only(),
            p(
                "SHARE",
                Kind::Name,
                false,
                Some("*NO"),
                "Share open data path",
            )
            .keyword_only(),
            p("OPNSCOPE", Kind::Name, false, Some("*NONE"), "Open scope").keyword_only(),
        ],
    },
    CommandDef {
        name: "DLTOVR",
        description: "Delete Override",
        params: &[
            p("FILE", Kind::List, true, None, "Overridden file").ch("Name, *ALL, *PRTF"),
            p("LVL", Kind::Name, false, Some("*"), "Call level").ch("*, *ACTGRPDFN, *JOB"),
        ],
    },
    CommandDef {
        name: "CLRPFM",
        description: "Clear Physical File Member",
        params: &[
            p("FILE", Kind::Qualified, true, None, "Physical file").ch("Name"),
            p("MBR", Kind::Name, false, Some("*FIRST"), "Member").ch("Name, *FIRST, *LAST"),
        ],
    },
    CommandDef {
        name: "DLYJOB",
        description: "Delay Job",
        params: &[
            p("DLY", Kind::Text, false, Some("*NONE"), "Delay time, or").ch("Seconds"),
            p("RSMTIME", Kind::Text, false, Some("*NONE"), "Resume time").ch("Time, *NONE"),
        ],
    },
    CommandDef {
        name: "CRTMSGF",
        description: "Create Message File",
        params: &[
            p("MSGF", Kind::Qualified, true, None, "Message file").ch("Name"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*BLANK"),
                "Text 'description'",
            ),
            p(
                "SIZE",
                Kind::List,
                false,
                Some("*NOMAX"),
                "Initial storage size",
            )
            .keyword_only(),
            p("AUT", Kind::Name, false, Some("*LIBCRTAUT"), "Authority").keyword_only(),
            p(
                "CCSID",
                Kind::Text,
                false,
                Some("*HEX"),
                "Coded character set ID",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "DLTMSGF",
        description: "Delete Message File",
        params: &[p("MSGF", Kind::Qualified, true, None, "Message file").ch("Name")],
    },
    CommandDef {
        name: "ADDMSGD",
        description: "Add Message Description",
        params: &[
            p("MSGID", Kind::Name, true, None, "Message identifier").ch("Name"),
            p("MSGF", Kind::Qualified, true, None, "Message file").ch("Name"),
            p("MSG", Kind::Text, true, None, "First-level message text"),
            p(
                "SECLVL",
                Kind::Text,
                false,
                Some("*NONE"),
                "Second-level message text",
            ),
            p("SEV", Kind::Text, false, Some("00"), "Severity code").ch("0-99"),
            p(
                "FMT",
                Kind::List,
                false,
                Some("*NONE"),
                "Message data fields formats",
            )
            .keyword_only(),
            p("TYPE", Kind::Name, false, Some("*NONE"), "Reply type").keyword_only(),
            p(
                "LEN",
                Kind::List,
                false,
                Some("*TYPE"),
                "Maximum reply length",
            )
            .keyword_only(),
            p(
                "VALUES",
                Kind::List,
                false,
                Some("*NONE"),
                "Valid reply values",
            )
            .keyword_only(),
            p(
                "DFT",
                Kind::Text,
                false,
                Some("*NONE"),
                "Default reply value",
            )
            .keyword_only(),
            p("ALROPT", Kind::List, false, Some("*NO"), "Alert options").keyword_only(),
            p("LOGPRB", Kind::Name, false, Some("*NO"), "Log problem").keyword_only(),
            p(
                "CCSID",
                Kind::Text,
                false,
                Some("*JOB"),
                "Coded character set ID",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CHGMSGD",
        description: "Change Message Description",
        params: &[
            p("MSGID", Kind::Name, true, None, "Message identifier").ch("Name"),
            p("MSGF", Kind::Qualified, true, None, "Message file").ch("Name"),
            p(
                "MSG",
                Kind::Text,
                false,
                Some("*SAME"),
                "First-level message text",
            ),
            p(
                "SECLVL",
                Kind::Text,
                false,
                Some("*SAME"),
                "Second-level message text",
            ),
            p("SEV", Kind::Text, false, Some("*SAME"), "Severity code").ch("0-99, *SAME"),
            p(
                "FMT",
                Kind::List,
                false,
                Some("*SAME"),
                "Message data fields formats",
            )
            .keyword_only(),
            p("TYPE", Kind::Name, false, Some("*SAME"), "Reply type").keyword_only(),
            p(
                "LEN",
                Kind::List,
                false,
                Some("*SAME"),
                "Maximum reply length",
            )
            .keyword_only(),
            p(
                "VALUES",
                Kind::List,
                false,
                Some("*SAME"),
                "Valid reply values",
            )
            .keyword_only(),
            p(
                "DFT",
                Kind::Text,
                false,
                Some("*SAME"),
                "Default reply value",
            )
            .keyword_only(),
            p("ALROPT", Kind::List, false, Some("*SAME"), "Alert options").keyword_only(),
            p("LOGPRB", Kind::Name, false, Some("*SAME"), "Log problem").keyword_only(),
            p(
                "CCSID",
                Kind::Text,
                false,
                Some("*SAME"),
                "Coded character set ID",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "RMVMSGD",
        description: "Remove Message Description",
        params: &[
            p("MSGID", Kind::Name, true, None, "Message identifier").ch("Name"),
            p("MSGF", Kind::Qualified, true, None, "Message file").ch("Name"),
        ],
    },
    CommandDef {
        name: "DSPMSGD",
        description: "Display Message Description",
        params: &[
            p(
                "RANGE",
                Kind::List,
                false,
                Some("*FIRST"),
                "Range of message identifiers",
            )
            .ch("Name, *FIRST, *ALL"),
            p(
                "MSGF",
                Kind::Qualified,
                false,
                Some("QCPFMSG"),
                "Message file",
            )
            .ch("Name"),
            p("DETAIL", Kind::Name, false, Some("*FULL"), "Detail").ch("*FULL, *BASIC"),
            p(
                "FMTTXT",
                Kind::Name,
                false,
                Some("*YES"),
                "Format message text",
            )
            .keyword_only(),
            p("OUTPUT", Kind::Name, false, Some("*"), "Output")
                .ch("*, *PRINT")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "DLTMNU",
        description: "Delete Menu",
        params: &[p("MENU", Kind::Qualified, true, None, "Menu").ch("Name")],
    },
    CommandDef {
        name: "DSPUSRPRF",
        description: "Display User Profile",
        params: &[p("USRPRF", Kind::Name, true, None, "User profile").ch("Name")],
    },
    CommandDef {
        name: "DLTUSRPRF",
        description: "Delete User Profile",
        params: &[p("USRPRF", Kind::Name, true, None, "User profile").ch("Name")],
    },
    CommandDef {
        name: "CHGPWD",
        description: "Change Password",
        params: &[],
    },
    CommandDef {
        name: "WRKOBJ",
        description: "Work with Objects",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name, generic*, *ALL"),
            p("OBJTYPE", Kind::Name, false, Some("*ALL"), "Object type")
                .ch("*ALL, *LIB, *FILE, *PGM, *DTAARA, *MENU"),
        ],
    },
    CommandDef {
        name: "SBMJOB",
        description: "Submit Job",
        params: &[
            p("CMD", Kind::Command, true, None, "Command to run"),
            p("JOB", Kind::Name, false, Some("*JOBD"), "Job name").ch("Name, *JOBD"),
            p("JOBQ", Kind::Name, false, Some("*JOBD"), "Job queue").ch("QBATCH, *JOBD"),
            p(
                "CURLIB",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Current library",
            )
            .ch("Name, *CURRENT, *CRTDFT")
            .keyword_only(),
            p(
                "INLLIBL",
                Kind::List,
                false,
                Some("*CURRENT"),
                "Initial library list",
            )
            .ch("Name, *CURRENT")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "WRKACTJOB",
        description: "Work with Active Jobs",
        params: &[],
    },
    CommandDef {
        name: "WRKSBMJOB",
        description: "Work with Submitted Jobs",
        params: &[],
    },
    CommandDef {
        name: "DSPJOBLOG",
        description: "Display Job Log",
        params: &[
            p("JOB", Kind::Text, false, Some("*"), "Job name").ch("*, number/user/name"),
            p("OUTPUT", Kind::Name, false, Some("*"), "Output").ch("*, *PRINT"),
        ],
    },
    CommandDef {
        name: "WRKSPLF",
        description: "Work with Spooled Files",
        params: &[
            p("SELECT", Kind::Name, false, Some("*CURRENT"), "User").ch("*CURRENT, user name")
        ],
    },
    CommandDef {
        name: "DSPSPLF",
        description: "Display Spooled File",
        params: &[
            p("FILE", Kind::Name, true, None, "Spooled file").ch("Name"),
            p("JOB", Kind::Text, false, Some("*"), "Job name").ch("*, number/user/name"),
            p(
                "SPLNBR",
                Kind::Text,
                false,
                Some("*LAST"),
                "Spooled file number",
            )
            .ch("1-999999, *LAST"),
        ],
    },
    CommandDef {
        name: "DLTSPLF",
        description: "Delete Spooled File",
        params: &[
            p("FILE", Kind::Name, true, None, "Spooled file").ch("Name"),
            p("JOB", Kind::Text, false, Some("*"), "Job name").ch("*, number/user/name"),
            p(
                "SPLNBR",
                Kind::Text,
                false,
                Some("*LAST"),
                "Spooled file number",
            )
            .ch("1-999999, *LAST"),
        ],
    },
    CommandDef {
        name: "ENDJOB",
        description: "End Job",
        params: &[p("JOB", Kind::Text, true, None, "Job name").ch("number/user/name")],
    },
    CommandDef {
        name: "SNDMSG",
        description: "Send Message",
        params: &[
            p("MSG", Kind::Text, true, None, "Message text"),
            p("TOUSR", Kind::Name, true, None, "To user profile").ch("Name"),
        ],
    },
    CommandDef {
        name: "DSPMSG",
        description: "Display Messages",
        params: &[
            p("MSGQ", Kind::Name, false, Some("*USRPRF"), "Message queue").ch("*USRPRF, user name"),
        ],
    },
    CommandDef {
        name: "GRTOBJAUT",
        description: "Grant Object Authority",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name"),
            p("OBJTYPE", Kind::Name, true, None, "Object type")
                .ch("*FILE, *PGM, *DTAARA, *MENU, *LIB"),
            p("USER", Kind::List, true, None, "Users").ch("Name, *PUBLIC"),
            p("AUT", Kind::Name, false, Some("*CHANGE"), "Authority")
                .ch("*CHANGE, *ALL, *USE, *EXCLUDE"),
        ],
    },
    CommandDef {
        name: "RVKOBJAUT",
        description: "Revoke Object Authority",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name"),
            p("OBJTYPE", Kind::Name, true, None, "Object type")
                .ch("*FILE, *PGM, *DTAARA, *MENU, *LIB"),
            p("USER", Kind::List, true, None, "Users").ch("Name, *PUBLIC"),
            p("AUT", Kind::Name, false, Some("*CHANGE"), "Authority").ch("*CHANGE, *ALL, *USE"),
        ],
    },
    CommandDef {
        name: "DSPOBJAUT",
        description: "Display Object Authority",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name"),
            p("OBJTYPE", Kind::Name, true, None, "Object type")
                .ch("*FILE, *PGM, *DTAARA, *MENU, *LIB"),
        ],
    },
    CommandDef {
        name: "CHGOBJOWN",
        description: "Change Object Owner",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name"),
            p("OBJTYPE", Kind::Name, true, None, "Object type")
                .ch("*FILE, *PGM, *DTAARA, *MENU, *LIB"),
            p("NEWOWN", Kind::Name, true, None, "New owner").ch("Name"),
        ],
    },
    CommandDef {
        name: "CRTDTAARA",
        description: "Create Data Area",
        params: &[
            p("DTAARA", Kind::Qualified, true, None, "Data area").ch("Name"),
            p("TYPE", Kind::Name, true, None, "Type").ch("*DEC, *CHAR, *LGL"),
            p("LEN", Kind::List, false, Some("*DFT"), "Length").ch("1-2000, digits decimals"),
            p("VALUE", Kind::Text, false, Some("*DFT"), "Initial value"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'"),
        ],
    },
    CommandDef {
        name: "CHGDTAARA",
        description: "Change Data Area",
        params: &[
            p("DTAARA", Kind::List, true, None, "Data area").ch("Name, (start length)"),
            p("VALUE", Kind::Text, true, None, "New value"),
        ],
    },
    CommandDef {
        name: "DSPDTAARA",
        description: "Display Data Area",
        params: &[p("DTAARA", Kind::Qualified, true, None, "Data area").ch("Name, *LDA")],
    },
    CommandDef {
        name: "DLTDTAARA",
        description: "Delete Data Area",
        params: &[p("DTAARA", Kind::Qualified, true, None, "Data area").ch("Name")],
    },
    CommandDef {
        name: "DLTF",
        description: "Delete File",
        params: &[p("FILE", Kind::Qualified, true, None, "File").ch("Name")],
    },
    CommandDef {
        name: "DSPPFM",
        description: "Display Physical File Member",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p("MBR", Kind::Name, false, Some("*FIRST"), "Member").ch("Name, *FIRST"),
        ],
    },
    CommandDef {
        name: "CRTSRCPF",
        description: "Create Source Physical File",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p("RCDLEN", Kind::Text, false, Some("92"), "Record length").ch("Number"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'").keyword_only(),
        ],
    },
    CommandDef {
        name: "CHGPFM",
        description: "Change Physical File Member",
        params: &[
            p("FILE", Kind::Qualified, true, None, "Physical file").ch("Name"),
            p("MBR", Kind::Name, true, None, "Member").ch("Name"),
            p("SRCTYPE", Kind::Name, false, Some("*SAME"), "Source type").ch("Name, *SAME, *NONE"),
            p(
                "EXPDATE",
                Kind::Name,
                false,
                Some("*SAME"),
                "Expiration date for member",
            )
            .ch("Date, *SAME, *NONE"),
            p(
                "SHARE",
                Kind::Name,
                false,
                Some("*SAME"),
                "Share open data path",
            )
            .ch("*SAME, *NO, *YES"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SAME"),
                "Text 'description'",
            ),
        ],
    },
    CommandDef {
        name: "DSPFD",
        description: "Display File Description",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p(
                "TYPE",
                Kind::Name,
                false,
                Some("*MBRLIST"),
                "Type of information",
            )
            .ch("*MBRLIST"),
        ],
    },
    CommandDef {
        name: "CRTPF",
        description: "Create Physical File",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QDDSSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*FILE"), "Source member").ch("Name, *FILE"),
        ],
    },
    CommandDef {
        name: "CRTLF",
        description: "Create Logical File",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QDDSSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*FILE"), "Source member").ch("Name, *FILE"),
        ],
    },
    CommandDef {
        name: "CRTDSPF",
        description: "Create Display File",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QDDSSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*FILE"), "Source member").ch("Name, *FILE"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .ch("")
            .keyword_only(),
            p("RSTDSP", Kind::Name, false, Some("*NO"), "Restore display")
                .ch("*NO, *YES")
                .keyword_only(),
            p("DFRWRT", Kind::Name, false, Some("*YES"), "Defer write")
                .ch("*YES, *NO")
                .keyword_only(),
            p("REPLACE", Kind::Name, false, Some("*YES"), "Replace file")
                .ch("*YES, *NO")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTPRTF",
        description: "Create Printer File",
        params: &[
            p("FILE", Kind::Qualified, true, None, "File").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("*NONE"),
                "Source file",
            )
            .ch("Name, *NONE"),
            p("SRCMBR", Kind::Name, false, Some("*FILE"), "Source member").ch("Name, *FILE"),
            p(
                "GENLVL",
                Kind::Text,
                false,
                Some("20"),
                "Generation severity level",
            )
            .keyword_only(),
            p(
                "FLAG",
                Kind::Text,
                false,
                Some("0"),
                "Flagging severity level",
            )
            .keyword_only(),
            p("DEV", Kind::Name, false, Some("*JOB"), "Device")
                .ch("Name, *JOB, *SYSVAL")
                .keyword_only(),
            p(
                "DEVTYPE",
                Kind::Name,
                false,
                Some("*SCS"),
                "Printer device type",
            )
            .ch("*SCS, *IPDS, *AFPDS, *USERASCII...")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p("PAGESIZE", Kind::List, false, Some("66 132"), "Page size")
                .ch("Length Width")
                .keyword_only(),
            p("LPI", Kind::Text, false, Some("6"), "Lines per inch").keyword_only(),
            p("CPI", Kind::Text, false, Some("10"), "Characters per inch").keyword_only(),
            p(
                "OVRFLW",
                Kind::Text,
                false,
                Some("60"),
                "Overflow line number",
            )
            .ch("1-255")
            .keyword_only(),
            p("OUTQ", Kind::Qualified, false, Some("*JOB"), "Output queue")
                .ch("Name, *JOB, *DEV")
                .keyword_only(),
            p("FORMTYPE", Kind::Name, false, Some("*STD"), "Form type").keyword_only(),
            p("COPIES", Kind::Text, false, Some("1"), "Copies").keyword_only(),
            p("HOLD", Kind::Name, false, Some("*NO"), "Hold spooled file")
                .ch("*NO, *YES")
                .keyword_only(),
            p("SAVE", Kind::Name, false, Some("*NO"), "Save spooled file")
                .ch("*NO, *YES")
                .keyword_only(),
            p("USRDTA", Kind::Text, false, Some("*SOURCE"), "User data").keyword_only(),
            p("REPLACE", Kind::Name, false, Some("*YES"), "Replace file")
                .ch("*YES, *NO")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTBNDCL",
        description: "Create Bound CL Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QCLSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*PGM"), "Source member").ch("Name, *PGM"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p(
                "DBGVIEW",
                Kind::Name,
                false,
                Some("*STMT"),
                "Debugging views",
            )
            .ch("*STMT, *SOURCE, *LIST, *COPY, *ALL, *NONE")
            .keyword_only(),
            p("OPTION", Kind::List, false, None, "Compiler options")
                .ch("*XREF, *NOXREF, *SRCSTMT, *NOSRCSTMT...")
                .keyword_only(),
            p(
                "OUTPUT",
                Kind::Name,
                false,
                Some("*PRINT"),
                "Compiler listing",
            )
            .ch("*PRINT, *NONE")
            .keyword_only(),
            p(
                "TGTRLS",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Target release",
            )
            .ch("*CURRENT, *PRV, VxRxMx")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTCLPGM",
        description: "Create CL Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QCLSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*PGM"), "Source member").ch("Name, *PGM"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTBNDRPG",
        description: "Create Bound RPG Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QRPGLESRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*PGM"), "Source member").ch("Name, *PGM"),
            p(
                "DFTACTGRP",
                Kind::Name,
                false,
                Some("*YES"),
                "Default activation group",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p(
                "BNDDIR",
                Kind::List,
                false,
                Some("*NONE"),
                "Binding directory",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "ACTGRP",
                Kind::Name,
                false,
                Some("*STGMDL"),
                "Activation group",
            )
            .ch("Name, *STGMDL, *CALLER, *NEW")
            .keyword_only(),
            p(
                "ALWNULL",
                Kind::Name,
                false,
                Some("*NO"),
                "Allow null values",
            )
            .ch("*NO, *INPUTONLY, *USRCTL, *YES")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p(
                "DBGVIEW",
                Kind::Name,
                false,
                Some("*STMT"),
                "Debugging views",
            )
            .ch("*STMT, *SOURCE, *LIST, *COPY, *ALL, *NONE")
            .keyword_only(),
            p("OPTION", Kind::List, false, None, "Compiler options")
                .ch("*XREF, *NOXREF, *SRCSTMT, *NOSRCSTMT...")
                .keyword_only(),
            p(
                "OUTPUT",
                Kind::Name,
                false,
                Some("*PRINT"),
                "Compiler listing",
            )
            .ch("*PRINT, *NONE")
            .keyword_only(),
            p(
                "TGTRLS",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Target release",
            )
            .ch("*CURRENT, *PRV, VxRxMx")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTCLMOD",
        description: "Create CL Module",
        params: &[
            p("MODULE", Kind::Qualified, true, None, "Module").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QCLSRC"),
                "Source file",
            )
            .ch("Name"),
            p(
                "SRCMBR",
                Kind::Name,
                false,
                Some("*MODULE"),
                "Source member",
            )
            .ch("Name, *MODULE"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p("REPLACE", Kind::Name, false, Some("*YES"), "Replace module")
                .ch("*YES, *NO")
                .keyword_only(),
            p(
                "DBGVIEW",
                Kind::Name,
                false,
                Some("*STMT"),
                "Debugging views",
            )
            .ch("*STMT, *SOURCE, *LIST, *COPY, *ALL, *NONE")
            .keyword_only(),
            p("OPTION", Kind::List, false, None, "Compiler options")
                .ch("*XREF, *NOXREF, *SRCSTMT, *NOSRCSTMT...")
                .keyword_only(),
            p(
                "OUTPUT",
                Kind::Name,
                false,
                Some("*PRINT"),
                "Compiler listing",
            )
            .ch("*PRINT, *NONE")
            .keyword_only(),
            p(
                "TGTRLS",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Target release",
            )
            .ch("*CURRENT, *PRV, VxRxMx")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CALLPRC",
        description: "Call Bound Procedure",
        params: &[
            p("PRC", Kind::Text, true, None, "Procedure").ch("Name"),
            p("PARM", Kind::Text, false, None, "Parameters"),
            p("RTNVAL", Kind::Text, false, Some("*NONE"), "Return value").ch("&variable, *NONE"),
        ],
    },
    CommandDef {
        name: "CRTRPGMOD",
        description: "Create RPG Module",
        params: &[
            p("MODULE", Kind::Qualified, true, None, "Module").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QRPGLESRC"),
                "Source file",
            )
            .ch("Name"),
            p(
                "SRCMBR",
                Kind::Name,
                false,
                Some("*MODULE"),
                "Source member",
            )
            .ch("Name, *MODULE"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p("REPLACE", Kind::Name, false, Some("*YES"), "Replace module")
                .ch("*YES, *NO")
                .keyword_only(),
            p(
                "DBGVIEW",
                Kind::Name,
                false,
                Some("*STMT"),
                "Debugging views",
            )
            .ch("*STMT, *SOURCE, *LIST, *COPY, *ALL, *NONE")
            .keyword_only(),
            p("OPTION", Kind::List, false, None, "Compiler options")
                .ch("*XREF, *NOXREF, *SRCSTMT, *NOSRCSTMT...")
                .keyword_only(),
            p(
                "OUTPUT",
                Kind::Name,
                false,
                Some("*PRINT"),
                "Compiler listing",
            )
            .ch("*PRINT, *NONE")
            .keyword_only(),
            p(
                "TGTRLS",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Target release",
            )
            .ch("*CURRENT, *PRV, VxRxMx")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTSRVPGM",
        description: "Create Service Program",
        params: &[
            p("SRVPGM", Kind::Qualified, true, None, "Service program").ch("Name"),
            p("MODULE", Kind::List, false, Some("*SRVPGM"), "Module").ch("Name, *SRVPGM, *NONE"),
            p("EXPORT", Kind::Name, false, Some("*SRCFILE"), "Export")
                .ch("*SRCFILE, *ALL")
                .keyword_only(),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QSRVSRC"),
                "Export source file",
            )
            .ch("Name")
            .keyword_only(),
            p(
                "SRCMBR",
                Kind::Name,
                false,
                Some("*SRVPGM"),
                "Export source member",
            )
            .ch("Name, *SRVPGM")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*BLANK"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "BNDSRVPGM",
                Kind::List,
                false,
                Some("*NONE"),
                "Bind service program",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "BNDDIR",
                Kind::List,
                false,
                Some("*NONE"),
                "Binding directory",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "ACTGRP",
                Kind::Name,
                false,
                Some("*CALLER"),
                "Activation group",
            )
            .ch("Name, *CALLER")
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace service program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTPGM",
        description: "Create Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p("MODULE", Kind::List, false, Some("*PGM"), "Module").ch("Name, *PGM"),
            p(
                "ENTMOD",
                Kind::Text,
                false,
                Some("*FIRST"),
                "Program entry procedure module",
            )
            .ch("Name, *FIRST")
            .keyword_only(),
            p(
                "BNDSRVPGM",
                Kind::List,
                false,
                Some("*NONE"),
                "Bind service program",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "BNDDIR",
                Kind::List,
                false,
                Some("*NONE"),
                "Binding directory",
            )
            .ch("Name, *NONE")
            .keyword_only(),
            p(
                "ACTGRP",
                Kind::Name,
                false,
                Some("*ENTMOD"),
                "Activation group",
            )
            .ch("Name, *ENTMOD, *NEW, *CALLER")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*ENTMODTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTBNDDIR",
        description: "Create Binding Directory",
        params: &[
            p("BNDDIR", Kind::Qualified, true, None, "Binding directory").ch("Name"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'").keyword_only(),
        ],
    },
    CommandDef {
        name: "ADDBNDDIRE",
        description: "Add Binding Directory Entry",
        params: &[
            p("BNDDIR", Kind::Qualified, true, None, "Binding directory").ch("Name"),
            p("OBJ", Kind::Command, true, None, "Object specifications")
                .ch("(Name *SRVPGM|*MODULE)..."),
        ],
    },
    CommandDef {
        name: "RMVBNDDIRE",
        description: "Remove Binding Directory Entry",
        params: &[
            p("BNDDIR", Kind::Qualified, true, None, "Binding directory").ch("Name"),
            p("OBJ", Kind::Command, true, None, "Object specifications")
                .ch("(Name *SRVPGM|*MODULE|*ALL)..."),
        ],
    },
    CommandDef {
        name: "DSPBNDDIR",
        description: "Display Binding Directory",
        params: &[p("BNDDIR", Kind::Qualified, true, None, "Binding directory").ch("Name")],
    },
    CommandDef {
        name: "DLTMOD",
        description: "Delete Module",
        params: &[p("MODULE", Kind::Qualified, true, None, "Module").ch("Name")],
    },
    CommandDef {
        name: "DLTSRVPGM",
        description: "Delete Service Program",
        params: &[p("SRVPGM", Kind::Qualified, true, None, "Service program").ch("Name")],
    },
    CommandDef {
        name: "DLTBNDDIR",
        description: "Delete Binding Directory",
        params: &[p("BNDDIR", Kind::Qualified, true, None, "Binding directory").ch("Name")],
    },
    CommandDef {
        name: "DSPPGM",
        description: "Display Program",
        params: &[p("PGM", Kind::Qualified, true, None, "Program").ch("Name")],
    },
    CommandDef {
        name: "DSPSRVPGM",
        description: "Display Service Program",
        params: &[p("SRVPGM", Kind::Qualified, true, None, "Service program").ch("Name")],
    },
    CommandDef {
        name: "UPDPGM",
        description: "Update Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p("MODULE", Kind::List, false, Some("*PGM"), "Module").ch("Name, *PGM, *NONE"),
        ],
    },
    CommandDef {
        name: "UPDSRVPGM",
        description: "Update Service Program",
        params: &[
            p("SRVPGM", Kind::Qualified, true, None, "Service program").ch("Name"),
            p("MODULE", Kind::List, false, Some("*SRVPGM"), "Module").ch("Name, *SRVPGM"),
            p("EXPORT", Kind::Name, false, Some("*SAME"), "Export")
                .ch("*SAME, *SRCFILE, *ALL")
                .keyword_only(),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QSRVSRC"),
                "Export source file",
            )
            .ch("Name")
            .keyword_only(),
            p(
                "SRCMBR",
                Kind::Name,
                false,
                Some("*SRVPGM"),
                "Export source member",
            )
            .ch("Name, *SRVPGM")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "RCLRSC",
        description: "Reclaim Resources",
        params: &[p("LVL", Kind::Name, false, Some("*"), "Level").ch("*, *CALLER")],
    },
    CommandDef {
        name: "RCLACTGRP",
        description: "Reclaim Activation Group",
        params: &[p("ACTGRP", Kind::Name, true, None, "Activation group").ch("Name, *ELIGIBLE")],
    },
    // Binder language (source type BND): read by CRTSRVPGM, not run.
    CommandDef {
        name: "STRPGMEXP",
        description: "Start Program Export List",
        params: &[
            p(
                "PGMLVL",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Program level",
            )
            .ch("*CURRENT, *PRV")
            .keyword_only(),
            p("LVLCHK", Kind::Name, false, Some("*YES"), "Level check")
                .ch("*YES, *NO")
                .keyword_only(),
            p("SIGNATURE", Kind::Text, false, Some("*GEN"), "Signature")
                .ch("*GEN, 'value'")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "EXPORT",
        description: "Export a Program Symbol",
        params: &[p("SYMBOL", Kind::Text, true, None, "Exported symbol name")],
    },
    CommandDef {
        name: "ENDPGMEXP",
        description: "End Program Export List",
        params: &[],
    },
    CommandDef {
        name: "CRTRPGPGM",
        description: "Create RPG/400 Program",
        params: &[
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QRPGSRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*PGM"), "Source member").ch("Name, *PGM"),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CVTRPGFREE",
        description: "Convert RPG to Free Form (as4tainr)",
        params: &[
            p("FROMFILE", Kind::Qualified, true, None, "From file").ch("Name"),
            p("FROMMBR", Kind::Name, true, None, "From member").ch("Name, generic*, *ALL"),
            p(
                "TOFILE",
                Kind::Qualified,
                false,
                Some("QRPGLESRC"),
                "To file",
            )
            .ch("Name"),
            p("TOMBR", Kind::Name, true, None, "To member").ch("Name, *FROMMBR"),
        ],
    },
    CommandDef {
        name: "CVTRPGSRC",
        description: "Convert RPG Source",
        params: &[
            p("FROMFILE", Kind::Qualified, true, None, "From file").ch("Name"),
            p("FROMMBR", Kind::Name, true, None, "From member").ch("Name, generic*, *ALL"),
            p(
                "TOFILE",
                Kind::Qualified,
                false,
                Some("QRPGLESRC"),
                "To file",
            )
            .ch("Name, *NONE"),
            p("TOMBR", Kind::Name, false, Some("*FROMMBR"), "To member").ch("Name, *FROMMBR"),
            p(
                "EXPCPY",
                Kind::Name,
                false,
                Some("*NO"),
                "Expand copy member",
            )
            .ch("*NO, *YES")
            .keyword_only(),
            p(
                "CVTRPT",
                Kind::Name,
                false,
                Some("*YES"),
                "Conversion report",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p(
                "LOGFILE",
                Kind::Qualified,
                false,
                Some("QRNCVTLG"),
                "Log file",
            )
            .ch("Name, *NONE")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTSQLRPGI",
        description: "Create SQL ILE RPG Object",
        params: &[
            p("OBJ", Kind::Qualified, true, None, "Object").ch("Name"),
            p(
                "SRCFILE",
                Kind::Qualified,
                false,
                Some("QRPGLESRC"),
                "Source file",
            )
            .ch("Name"),
            p("SRCMBR", Kind::Name, false, Some("*OBJ"), "Source member").ch("Name, *OBJ"),
            p(
                "COMMIT",
                Kind::Name,
                false,
                Some("*CHG"),
                "Commitment control",
            )
            .ch("*CHG, *ALL, *CS, *NONE, *RR")
            .keyword_only(),
            p("OBJTYPE", Kind::Name, false, Some("*PGM"), "Type of object")
                .ch("*PGM, *SRVPGM, *MODULE")
                .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SRCMBRTXT"),
                "Text 'description'",
            )
            .keyword_only(),
            p(
                "REPLACE",
                Kind::Name,
                false,
                Some("*YES"),
                "Replace program",
            )
            .ch("*YES, *NO")
            .keyword_only(),
            p(
                "DBGVIEW",
                Kind::Name,
                false,
                Some("*NONE"),
                "Debugging views",
            )
            .ch("*STMT, *SOURCE, *LIST, *COPY, *ALL, *NONE")
            .keyword_only(),
            p("OPTION", Kind::List, false, None, "Compiler options")
                .ch("*XREF, *NOXREF, *SRCSTMT, *NOSRCSTMT...")
                .keyword_only(),
            p(
                "OUTPUT",
                Kind::Name,
                false,
                Some("*PRINT"),
                "Compiler listing",
            )
            .ch("*PRINT, *NONE")
            .keyword_only(),
            p(
                "TGTRLS",
                Kind::Name,
                false,
                Some("*CURRENT"),
                "Target release",
            )
            .ch("*CURRENT, *PRV, VxRxMx")
            .keyword_only(),
            p(
                "RPGPPOPT",
                Kind::Name,
                false,
                Some("*NONE"),
                "RPG preprocessor options",
            )
            .ch("*NONE, *LVL1, *LVL2")
            .keyword_only(),
            p(
                "CLOSQLCSR",
                Kind::Name,
                false,
                Some("*ENDACTGRP"),
                "Close SQL cursor",
            )
            .ch("*ENDACTGRP, *ENDMOD")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "MKDIR",
        description: "Make Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p(
                "DTAAUT",
                Kind::Name,
                false,
                Some("*INDIR"),
                "Public authority for data",
            )
            .ch("*INDIR, *RWX, *RW, *RX, *WX, *R, *W, *X, *EXCLUDE, *NONE"),
            p(
                "OBJAUT",
                Kind::List,
                false,
                Some("*INDIR"),
                "Public authority for object",
            )
            .ch("*INDIR, *NONE, *ALL, *OBJEXIST, *OBJMGT, *OBJALTER, *OBJREF"),
        ],
    },
    CommandDef {
        name: "MD",
        description: "Make Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p(
                "DTAAUT",
                Kind::Name,
                false,
                Some("*INDIR"),
                "Public authority for data",
            )
            .ch("*INDIR, *RWX, *RW, *RX, *WX, *R, *W, *X, *EXCLUDE, *NONE"),
            p(
                "OBJAUT",
                Kind::List,
                false,
                Some("*INDIR"),
                "Public authority for object",
            )
            .ch("*INDIR, *NONE, *ALL, *OBJEXIST, *OBJMGT, *OBJALTER, *OBJREF"),
        ],
    },
    CommandDef {
        name: "CRTDIR",
        description: "Create Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p(
                "DTAAUT",
                Kind::Name,
                false,
                Some("*INDIR"),
                "Public authority for data",
            )
            .ch("*INDIR, *RWX, *RW, *RX, *WX, *R, *W, *X, *EXCLUDE, *NONE"),
            p(
                "OBJAUT",
                Kind::List,
                false,
                Some("*INDIR"),
                "Public authority for object",
            )
            .ch("*INDIR, *NONE, *ALL, *OBJEXIST, *OBJMGT, *OBJALTER, *OBJREF"),
        ],
    },
    CommandDef {
        name: "RMVDIR",
        description: "Remove Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p("RMVLNK", Kind::Name, false, Some("*NO"), "Remove link").ch("*NO, *YES"),
            p("SUBTREE", Kind::Name, false, Some("*NONE"), "Subtree").ch("*NONE, *ALL"),
        ],
    },
    CommandDef {
        name: "RMDIR",
        description: "Remove Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p("RMVLNK", Kind::Name, false, Some("*NO"), "Remove link").ch("*NO, *YES"),
            p("SUBTREE", Kind::Name, false, Some("*NONE"), "Subtree").ch("*NONE, *ALL"),
        ],
    },
    CommandDef {
        name: "RD",
        description: "Remove Directory",
        params: &[
            p("DIR", Kind::Text, true, None, "Directory").ch("Path name"),
            p("RMVLNK", Kind::Name, false, Some("*NO"), "Remove link").ch("*NO, *YES"),
            p("SUBTREE", Kind::Name, false, Some("*NONE"), "Subtree").ch("*NONE, *ALL"),
        ],
    },
    CommandDef {
        name: "RMVLNK",
        description: "Remove Link",
        params: &[p("OBJLNK", Kind::Text, true, None, "Object link").ch("Path name, generic*")],
    },
    CommandDef {
        name: "DEL",
        description: "Delete File",
        params: &[p("OBJLNK", Kind::Text, true, None, "Object link").ch("Path name, generic*")],
    },
    CommandDef {
        name: "ERASE",
        description: "Erase File",
        params: &[p("OBJLNK", Kind::Text, true, None, "Object link").ch("Path name, generic*")],
    },
    CommandDef {
        name: "RNM",
        description: "Rename Object",
        params: &[
            p("OBJ", Kind::Text, true, None, "Object").ch("Path name"),
            p("NEWOBJ", Kind::Text, true, None, "New object").ch("Name"),
        ],
    },
    CommandDef {
        name: "WRKLNK",
        description: "Work with Object Links",
        params: &[p("OBJ", Kind::Text, false, Some("*"), "Object").ch("Path name, *")],
    },
    CommandDef {
        name: "DSPLNK",
        description: "Display Object Links",
        params: &[p("OBJ", Kind::Text, false, Some("*"), "Object").ch("Path name, *")],
    },
    CommandDef {
        name: "CPYTOSTMF",
        description: "Copy To Stream File",
        params: &[
            p("FROMMBR", Kind::Text, true, None, "From member").ch("Path name"),
            p("TOSTMF", Kind::Text, true, None, "To stream file").ch("Path name"),
            p(
                "STMFOPT",
                Kind::Name,
                false,
                Some("*NONE"),
                "Stream file option",
            )
            .ch("*NONE, *ADD, *REPLACE"),
            p(
                "CVTDTA",
                Kind::Name,
                false,
                Some("*AUTO"),
                "Data conversion options",
            )
            .ch("*AUTO, *TBL, *NONE"),
            p(
                "DBFCCSID",
                Kind::Text,
                false,
                Some("*FILE"),
                "Database file CCSID",
            )
            .ch("1-65533, *FILE"),
            p(
                "STMFCCSID",
                Kind::Text,
                false,
                Some("*STMF"),
                "Stream file CCSID",
            )
            .ch("1-65533, *STMF, *PCASCII, *STDASCII"),
            p(
                "ENDLINFMT",
                Kind::Name,
                false,
                Some("*CRLF"),
                "End of line characters",
            )
            .ch("*CRLF, *LF, *CR, *LFCR, *FIXED"),
            p("AUT", Kind::Name, false, Some("*DFT"), "Authority")
                .ch("*DFT, *INDIR, *FILE, *INDIRFILE"),
        ],
    },
    CommandDef {
        name: "CPYFRMSTMF",
        description: "Copy From Stream File",
        params: &[
            p("FROMSTMF", Kind::Text, true, None, "From stream file").ch("Path name"),
            p("TOMBR", Kind::Text, true, None, "To member").ch("Path name"),
            p(
                "MBROPT",
                Kind::Name,
                false,
                Some("*NONE"),
                "Replace or add records",
            )
            .ch("*NONE, *ADD, *REPLACE"),
            p(
                "CVTDTA",
                Kind::Name,
                false,
                Some("*AUTO"),
                "Data conversion options",
            )
            .ch("*AUTO, *TBL, *NONE"),
            p(
                "STMFCCSID",
                Kind::Text,
                false,
                Some("*STMF"),
                "Stream file CCSID",
            )
            .ch("1-65533, *STMF, *PCASCII"),
            p(
                "DBFCCSID",
                Kind::Text,
                false,
                Some("*FILE"),
                "Database file CCSID",
            )
            .ch("1-65533, *FILE"),
            p(
                "ENDLINFMT",
                Kind::Name,
                false,
                Some("*ALL"),
                "End of line characters",
            )
            .ch("*ALL, *CRLF, *LF, *CR, *LFCR, *FIXED"),
            p(
                "TABEXPN",
                Kind::Name,
                false,
                Some("*YES"),
                "Tab character expansion",
            )
            .ch("*YES, *NO"),
        ],
    },
    CommandDef {
        name: "CRTJRNRCV",
        description: "Create Journal Receiver",
        params: &[
            p("JRNRCV", Kind::Qualified, true, None, "Journal receiver").ch("Name"),
            p("ASP", Kind::Name, false, Some("*LIBASP"), "ASP number").keyword_only(),
            p(
                "THRESHOLD",
                Kind::Text,
                false,
                Some("1500000"),
                "Journal receiver threshold",
            )
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*BLANK"),
                "Text 'description'",
            )
            .keyword_only(),
            p("AUT", Kind::Name, false, Some("*LIBCRTAUT"), "Authority").keyword_only(),
        ],
    },
    CommandDef {
        name: "CRTJRN",
        description: "Create Journal",
        params: &[
            p("JRN", Kind::Qualified, true, None, "Journal").ch("Name"),
            p("JRNRCV", Kind::Qualified, true, None, "Journal receiver").ch("Name"),
            p(
                "MNGRCV",
                Kind::Name,
                false,
                Some("*SYSTEM"),
                "Manage receivers",
            )
            .ch("*SYSTEM, *USER")
            .keyword_only(),
            p("DLTRCV", Kind::Name, false, Some("*NO"), "Delete receivers")
                .ch("*NO, *YES")
                .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*BLANK"),
                "Text 'description'",
            )
            .keyword_only(),
            p("AUT", Kind::Name, false, Some("*LIBCRTAUT"), "Authority").keyword_only(),
        ],
    },
    CommandDef {
        name: "CHGJRN",
        description: "Change Journal",
        params: &[
            p("JRN", Kind::Qualified, true, None, "Journal").ch("Name"),
            p(
                "JRNRCV",
                Kind::Qualified,
                false,
                Some("*SAME"),
                "Journal receiver",
            )
            .ch("Name, *SAME, *GEN"),
            p(
                "SEQOPT",
                Kind::Name,
                false,
                Some("*CONT"),
                "Sequence option",
            )
            .ch("*CONT, *RESET")
            .keyword_only(),
            p(
                "TEXT",
                Kind::Text,
                false,
                Some("*SAME"),
                "Text 'description'",
            )
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "DLTJRN",
        description: "Delete Journal",
        params: &[p("JRN", Kind::Qualified, true, None, "Journal").ch("Name")],
    },
    CommandDef {
        name: "DLTJRNRCV",
        description: "Delete Journal Receiver",
        params: &[
            p("JRNRCV", Kind::Qualified, true, None, "Journal receiver").ch("Name"),
            p("DLTOPT", Kind::List, false, Some("*NONE"), "Option").keyword_only(),
        ],
    },
    CommandDef {
        name: "STRJRNPF",
        description: "Start Journal Physical File",
        params: &[
            p(
                "FILE",
                Kind::List,
                true,
                None,
                "Physical file to be journaled",
            )
            .ch("Name, generic*, *ALL"),
            p("JRN", Kind::Qualified, true, None, "Journal").ch("Name"),
            p("IMAGES", Kind::Name, false, Some("*AFTER"), "Record images")
                .ch("*AFTER, *BOTH")
                .keyword_only(),
            p(
                "OMTJRNE",
                Kind::Name,
                false,
                Some("*NONE"),
                "Journal entries to be omitted",
            )
            .ch("*NONE, *OPNCLO")
            .keyword_only(),
            p(
                "LOGLVL",
                Kind::Name,
                false,
                Some("*ERRORS"),
                "Logging level",
            )
            .ch("*ERRORS, *ALL")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "ENDJRNPF",
        description: "End Journal Physical File",
        params: &[
            p("FILE", Kind::List, true, None, "Journaled physical file").ch("Name, generic*, *ALL"),
            p("JRN", Kind::Qualified, false, Some("*FILE"), "Journal").ch("Name, *FILE"),
            p(
                "LOGLVL",
                Kind::Name,
                false,
                Some("*ERRORS"),
                "Logging level",
            )
            .ch("*ERRORS, *ALL")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "DSPJRN",
        description: "Display Journal",
        params: &[
            p("JRN", Kind::Qualified, true, None, "Journal").ch("Name"),
            p(
                "FILE",
                Kind::List,
                false,
                Some("*ALLFILE"),
                "Journaled physical file",
            )
            .ch("Name, *ALLFILE, *ALL"),
            p(
                "RCVRNG",
                Kind::List,
                false,
                Some("*CURRENT"),
                "Range of journal receivers",
            )
            .ch("*CURRENT, *CURCHAIN")
            .keyword_only(),
            p("JRNCDE", Kind::List, false, Some("*ALL"), "Journal codes")
                .ch("*ALL, *CTL, code")
                .keyword_only(),
            p(
                "ENTTYP",
                Kind::List,
                false,
                Some("*ALL"),
                "Journal entry types",
            )
            .ch("*ALL, *RCD, type")
            .keyword_only(),
            p("OUTPUT", Kind::Name, false, Some("*"), "Output")
                .ch("*, *PRINT")
                .keyword_only(),
        ],
    },
    CommandDef {
        name: "ADDPFTRG",
        description: "Add Physical File Trigger",
        params: &[
            p("FILE", Kind::Qualified, true, None, "Physical file").ch("Name"),
            p("TRGTIME", Kind::Name, true, None, "Trigger time").ch("*BEFORE, *AFTER"),
            p("TRGEVENT", Kind::Name, true, None, "Trigger event")
                .ch("*INSERT, *DELETE, *UPDATE, *READ"),
            p("PGM", Kind::Qualified, true, None, "Program").ch("Name"),
            p("RPLTRG", Kind::Name, false, Some("*NO"), "Replace trigger").ch("*NO, *YES"),
            p("TRG", Kind::Text, false, Some("*GEN"), "Trigger")
                .ch("Name, *GEN")
                .keyword_only(),
            p(
                "TRGLIB",
                Kind::Name,
                false,
                Some("*FILE"),
                "Trigger library",
            )
            .ch("Name, *FILE, *CURLIB")
            .keyword_only(),
            p(
                "ALWREPCHG",
                Kind::Name,
                false,
                Some("*NO"),
                "Allow repeated change",
            )
            .ch("*NO, *YES")
            .keyword_only(),
            p("THDSAFE", Kind::Name, false, Some("*UNKNOWN"), "Threadsafe")
                .ch("*UNKNOWN, *NO, *YES")
                .keyword_only(),
            p(
                "MLTTHDACN",
                Kind::Name,
                false,
                Some("*SYSVAL"),
                "Multithreaded job action",
            )
            .ch("*SYSVAL, *MSG, *NORUN, *RUN")
            .keyword_only(),
            p(
                "TRGUPDCND",
                Kind::Name,
                false,
                Some("*ALWAYS"),
                "Trigger update condition",
            )
            .ch("*ALWAYS, *CHANGE")
            .keyword_only(),
        ],
    },
    CommandDef {
        name: "RMVPFTRG",
        description: "Remove Physical File Trigger",
        params: &[
            p("FILE", Kind::Qualified, true, None, "Physical file").ch("Name"),
            p("TRGTIME", Kind::Name, false, Some("*ALL"), "Trigger time")
                .ch("*ALL, *BEFORE, *AFTER"),
            p("TRGEVENT", Kind::Name, false, Some("*ALL"), "Trigger event")
                .ch("*ALL, *INSERT, *DELETE, *UPDATE, *READ"),
            p("TRG", Kind::Text, false, Some("*ALL"), "Trigger")
                .ch("Name, *ALL")
                .keyword_only(),
            p("TRGLIB", Kind::Name, false, Some("*ALL"), "Trigger library").keyword_only(),
        ],
    },
];

/// A group of commands on the F4 menu.
pub struct Group {
    /// The line on the Command Groups menu.
    pub item: &'static str,
    /// The title of the group's own menu.
    pub title: &'static str,
    /// Its commands, in menu order.
    pub commands: &'static [&'static str],
}

pub static GROUPS: &[Group] = &[
    Group {
        item: "Library commands",
        title: "Library Commands",
        commands: &[
            "CRTLIB",
            "DSPLIB",
            "DSPLIBL",
            "ADDLIBLE",
            "RMVLIBLE",
            "CHGCURLIB",
            "CHGLIBL",
        ],
    },
    Group {
        item: "Object commands",
        title: "Object Commands",
        commands: &["WRKOBJ", "CRTDTAARA", "CHGDTAARA", "DSPDTAARA", "DLTDTAARA"],
    },
    Group {
        item: "File commands",
        title: "File Commands",
        commands: &["DSPPFM", "DLTF"],
    },
    Group {
        item: "User profile commands",
        title: "User Profile Commands",
        commands: &["CRTUSRPRF", "DSPUSRPRF", "DLTUSRPRF", "CHGPWD", "CHGUSRPRF"],
    },
    Group {
        item: "Security commands",
        title: "Security Commands",
        commands: &["GRTOBJAUT", "RVKOBJAUT", "DSPOBJAUT", "CHGOBJOWN"],
    },
    Group {
        item: "SQL commands",
        title: "SQL Commands",
        commands: &["STRSQL", "RUNSQL", "RUNSQLSTM"],
    },
    Group {
        item: "Program commands",
        title: "Program Commands",
        commands: &["CALL", "CALLPRC"],
    },
    Group {
        item: "Job and message commands",
        title: "Job and Message Commands",
        commands: &[
            "SBMJOB",
            "WRKACTJOB",
            "WRKSBMJOB",
            "DSPJOBLOG",
            "ENDJOB",
            "SNDMSG",
            "DSPMSG",
            "DLYJOB",
        ],
    },
    Group {
        item: "Spooled file commands",
        title: "Spooled File Commands",
        commands: &["WRKSPLF", "DSPSPLF", "DLTSPLF"],
    },
    Group {
        item: "Source and create commands",
        title: "Source and Create Commands",
        commands: &[
            "CRTSRCPF",
            "CHGPFM",
            "DSPFD",
            "CRTPF",
            "CRTLF",
            "CRTDSPF",
            "CRTPRTF",
            "CRTBNDCL",
            "CRTCLPGM",
            "CRTBNDRPG",
            "CRTSQLRPGI",
            "CRTCLMOD",
            "CRTRPGPGM",
            "CVTRPGSRC",
            "CVTRPGFREE",
            "CRTRPGMOD",
            "CRTSRVPGM",
            "CRTPGM",
            "CRTBNDDIR",
            "ADDBNDDIRE",
            "RMVBNDDIRE",
            "DSPBNDDIR",
            "DLTMOD",
            "DLTSRVPGM",
            "DLTBNDDIR",
            "RCLACTGRP",
            "RCLRSC",
            "DSPPGM",
            "DSPSRVPGM",
            "UPDPGM",
            "UPDSRVPGM",
        ],
    },
    Group {
        item: "Command definition commands",
        title: "Command Definition Commands",
        commands: &["CRTCMD", "DLTCMD", "DSPCMD"],
    },
    Group {
        item: "File override and commitment commands",
        title: "File Override and Commitment Commands",
        commands: &[
            "OVRDBF",
            "OVRPRTF",
            "DLTOVR",
            "CLRPFM",
            "STRCMTCTL",
            "ENDCMTCTL",
            "COMMIT",
            "ROLLBACK",
        ],
    },
    Group {
        item: "Message file commands",
        title: "Message File Commands",
        commands: &[
            "CRTMSGF", "DLTMSGF", "ADDMSGD", "CHGMSGD", "RMVMSGD", "DSPMSGD",
        ],
    },
    Group {
        item: "Binder language",
        title: "Binder Language Commands",
        commands: &["STRPGMEXP", "EXPORT", "ENDPGMEXP"],
    },
    Group {
        item: "Menu commands",
        title: "Menu Commands",
        commands: &["GO", "CRTMNU", "DLTMNU"],
    },
    Group {
        item: "Session commands",
        title: "Session Commands",
        commands: &["SIGNOFF"],
    },
    Group {
        item: "Integrated file system commands",
        title: "Integrated File System Commands",
        commands: &[
            "WRKLNK",
            "DSPLNK",
            "MKDIR",
            "MD",
            "CRTDIR",
            "RMVDIR",
            "RMDIR",
            "RD",
            "RMVLNK",
            "DEL",
            "ERASE",
            "RNM",
            "CPYTOSTMF",
            "CPYFRMSTMF",
        ],
    },
    Group {
        item: "Journal commands",
        title: "Journal Commands",
        commands: &[
            "CRTJRNRCV",
            "CRTJRN",
            "CHGJRN",
            "DLTJRN",
            "DLTJRNRCV",
            "STRJRNPF",
            "ENDJRNPF",
            "DSPJRN",
        ],
    },
    Group {
        item: "Trigger commands",
        title: "Trigger Commands",
        commands: &["ADDPFTRG", "RMVPFTRG"],
    },
];

/// Library part of a qualified name: special values allowed.
const LIBRARY_SPECIALS: &[&str] = &["*LIBL", "*CURLIB", "*ALL", "*USRLIBL", "*ALLUSR"];

pub fn find(name: &str) -> Option<&'static CommandDef> {
    // A qualified name (QSYS/CRTLIB, MYLIB/MYCMD): the command's name.
    let name = name.rsplit('/').next().unwrap_or(name);
    COMMANDS
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .or_else(|| {
            USER_COMMANDS
                .lock()
                .ok()?
                .as_ref()?
                .get(&name.to_ascii_uppercase())
                .copied()
        })
}

/// A system command (in the table), not a user-defined one.
pub fn is_system(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.name.eq_ignore_ascii_case(name))
}

/// Commands made with CRTCMD, as the prompter last looked them up.
static USER_COMMANDS: std::sync::Mutex<
    Option<std::collections::HashMap<String, &'static CommandDef>>,
> = std::sync::Mutex::new(None);

/// Make a user command known to the parser and the prompter.
pub fn register_user(def: &'static CommandDef) {
    if let Ok(mut g) = USER_COMMANDS.lock() {
        g.get_or_insert_with(Default::default)
            .insert(def.name.to_ascii_uppercase(), def);
    }
}

/// A parsed value: unquoted text is upper-cased, quoted text is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Word(String),
    Quoted(String),
}

impl Value {
    pub fn text(&self) -> &str {
        match self {
            Value::Word(s) | Value::Quoted(s) => s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub def: &'static CommandDef,
    /// Values by keyword. Defaults are filled in for omitted parameters.
    pub params: BTreeMap<&'static str, Vec<Value>>,
}

impl Command {
    /// The single value of a parameter, or "" if omitted with no default.
    pub fn get(&self, keyword: &str) -> &str {
        self.params
            .get(keyword)
            .and_then(|v| v.first())
            .map(|v| v.text())
            .unwrap_or("")
    }

    pub fn list(&self, keyword: &str) -> Vec<String> {
        self.params
            .get(keyword)
            .map(|v| v.iter().map(|x| x.text().to_string()).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Quoted(String),
    Open,
    Close,
}

fn tokenize(s: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' => i += 1,
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            '\'' => {
                let mut v = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => bail!(
                            "CPF0006: Errors occurred in command. A closing apostrophe is missing."
                        ),
                        Some('\'') if cs.get(i + 1) == Some(&'\'') => {
                            v.push('\'');
                            i += 2;
                        }
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            v.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Quoted(v));
            }
            _ => {
                let start = i;
                while i < cs.len() && !matches!(cs[i], ' ' | '\t' | '(' | ')' | '\'') {
                    i += 1;
                }
                out.push(Tok::Word(
                    cs[start..i].iter().collect::<String>().to_ascii_uppercase(),
                ));
            }
        }
    }
    Ok(out)
}

/// A value that is itself a command (`SBMJOB CMD(CALL PGM(X) PARM('a'))`)
/// is turned into one quoted string before tokenizing, so its own
/// parentheses and keywords stay as written.
fn quote_command_values(line: &str) -> Result<String> {
    let name = line
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    let Some(def) = find(name) else {
        return Ok(line.to_string());
    };
    let mut out = line.to_string();
    for pd in def.params.iter().filter(|p| p.kind == Kind::Command) {
        let cs: Vec<char> = out.chars().collect();
        let kw: Vec<char> = pd.keyword.chars().collect();
        let mut i = 0;
        let mut in_q = false;
        let mut found = None;
        while i < cs.len() {
            if cs[i] == '\'' {
                in_q = !in_q;
            } else if !in_q
                && (i == 0 || cs[i - 1].is_whitespace())
                && cs.len() >= i + kw.len()
                && cs[i..i + kw.len()]
                    .iter()
                    .zip(&kw)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                let mut j = i + kw.len();
                while j < cs.len() && cs[j] == ' ' {
                    j += 1;
                }
                if cs.get(j) == Some(&'(') {
                    found = Some((i, j));
                    break;
                }
            }
            i += 1;
        }
        let Some((start, open)) = found else { continue };
        // The matching close parenthesis, outside quotes.
        let (mut depth, mut q, mut close) = (0i32, false, None);
        for (k, &c) in cs.iter().enumerate().skip(open) {
            match c {
                '\'' => q = !q,
                '(' if !q => depth += 1,
                ')' if !q => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            return Err(err(format!(
                "A closing parenthesis is missing after {}.",
                pd.keyword
            )));
        };
        let inner: String = cs[open + 1..close].iter().collect();
        let quoted = format!("{}('{}')", pd.keyword, inner.trim().replace('\'', "''"));
        out = format!(
            "{}{}{}",
            cs[..start].iter().collect::<String>(),
            quoted,
            cs[close + 1..].iter().collect::<String>()
        );
    }
    Ok(out)
}

fn err(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("CPF0006: Errors occurred in command. {msg}")
}

/// Parse a command without checking for required parameters or filling in
/// defaults: what F4 prompting starts from.
pub fn parse_partial(
    line: &str,
) -> Result<(&'static CommandDef, BTreeMap<&'static str, Vec<Value>>)> {
    let line = quote_command_values(line.trim())?;
    let toks = tokenize(&line)?;
    let mut it = toks.into_iter().peekable();
    let name = match it.next() {
        Some(Tok::Word(w)) => w,
        _ => bail!("CPF0006: Errors occurred in command. A command name is expected."),
    };
    let Some(def) = find(&name) else {
        bail!("CPD0030: Command {name} in library *LIBL not found.");
    };
    let mut params: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut next_positional = def.params.iter().filter(|p| p.positional);

    while let Some(tok) = it.next() {
        match tok {
            Tok::Word(w) if it.peek() == Some(&Tok::Open) => {
                it.next();
                let Some(pd) = def.params.iter().find(|p| p.keyword == w) else {
                    return Err(err(format!(
                        "Keyword {w} not valid for command {}.",
                        def.name
                    )));
                };
                let mut vals = Vec::new();
                loop {
                    match it.next() {
                        Some(Tok::Close) => break,
                        Some(Tok::Word(v)) => vals.push(Value::Word(v)),
                        Some(Tok::Quoted(v)) => vals.push(Value::Quoted(v)),
                        // An element list inside a value, e.g. the
                        // substring in DTAARA(NAME (1 10)): kept as one
                        // value "(1 10)" for the command to take apart.
                        Some(Tok::Open) => {
                            // Lists in lists (CALL PARM(('x' (*CHAR 128)))):
                            // the whole group as written.
                            let mut text = String::from("(");
                            let mut depth = 1;
                            while depth > 0 {
                                match it.next() {
                                    Some(Tok::Close) => {
                                        depth -= 1;
                                        text = text.trim_end().to_string();
                                        text.push_str(") ");
                                    }
                                    Some(Tok::Open) => {
                                        depth += 1;
                                        // Only CALL's PARM takes a list
                                        // inside an element list: a value
                                        // and its (type length).
                                        let typed = pd.keyword == "PARM"
                                            && matches!(def.name, "CALL" | "TFRCTL" | "CALLPRC");
                                        if depth > 2 || !typed {
                                            return Err(err(format!(
                                                "Lists nested more than two deep are not supported in {w}."
                                            )));
                                        }
                                        text.push('(');
                                    }
                                    Some(Tok::Word(v)) => {
                                        text.push_str(&v);
                                        text.push(' ');
                                    }
                                    Some(Tok::Quoted(v)) => {
                                        text.push_str(&format!("'{}' ", v.replace('\'', "''")));
                                    }
                                    None => {
                                        return Err(err(format!(
                                            "A closing parenthesis is missing after {w}."
                                        )))
                                    }
                                }
                            }
                            vals.push(Value::Word(text.trim_end().to_string()));
                        }
                        None => {
                            return Err(err(format!("A closing parenthesis is missing after {w}.")))
                        }
                    }
                }
                if params.insert(pd.keyword, vals).is_some() {
                    return Err(err(format!("Keyword {w} specified more than once.")));
                }
            }
            Tok::Word(_) | Tok::Quoted(_) => {
                let v = match tok {
                    Tok::Word(s) => Value::Word(s),
                    Tok::Quoted(s) => Value::Quoted(s),
                    _ => unreachable!(),
                };
                let Some(pd) = next_positional.find(|p| !params.contains_key(p.keyword)) else {
                    return Err(err(format!(
                        "Too many positional parameters for command {}.",
                        def.name
                    )));
                };
                params.insert(pd.keyword, vec![v]);
            }
            Tok::Open | Tok::Close => return Err(err("Unexpected parenthesis.")),
        }
    }
    Ok((def, params))
}

fn check_qualified(pd: &ParamDef, v: &str) -> Result<()> {
    let (lib, obj) = match v.split_once('/') {
        Some((l, o)) => (Some(l), o),
        None => (None, v),
    };
    if let Some(l) = lib {
        if !LIBRARY_SPECIALS.contains(&l) {
            crate::catalog::validate_name("library", l).map_err(err)?;
        }
    }
    let what = pd.prompt.to_ascii_lowercase();
    // Special values the parameter allows (*ALL, *NONE, *SAME...).
    if obj.starts_with('*')
        && pd
            .choices
            .split(", ")
            .any(|c| c.trim_end_matches("...") == obj)
    {
        return Ok(());
    }
    // Generic names: a valid name prefix followed by '*'.
    let base = match obj.strip_suffix('*') {
        Some(b) if pd.choices.contains("generic") && !b.is_empty() => b,
        _ => obj,
    };
    crate::catalog::validate_name(&what, base).map_err(err)?;
    Ok(())
}

pub fn parse(line: &str) -> Result<Command> {
    let (def, mut params) = parse_partial(line)?;
    for pd in def.params {
        match params.get(pd.keyword) {
            Some(v) if pd.kind != Kind::List && v.len() > 1 => {
                return Err(err(format!("Too many values for keyword {}.", pd.keyword)));
            }
            Some(_) => {}
            None if pd.required => {
                return Err(err(format!(
                    "Value for required parameter {} is missing.",
                    pd.keyword
                )));
            }
            None => {
                if let Some(d) = pd.default {
                    params.insert(pd.keyword, vec![Value::Word(d.to_string())]);
                }
            }
        }
        if let Some(v) = params.get(pd.keyword).and_then(|v| v.first()) {
            let t = v.text();
            match pd.kind {
                Kind::Name if !t.starts_with('*') && !t.is_empty() => {
                    crate::catalog::validate_name(&pd.prompt.to_ascii_lowercase(), t)
                        .map_err(err)?;
                }
                Kind::Qualified => check_qualified(pd, t)?,
                _ => {}
            }
        }
    }
    Ok(Command { def, params })
}

/// One value as CL source: special values and names as they are, anything
/// else quoted with apostrophes doubled.
fn cl_value(kind: Kind, v: &str) -> String {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "*$#@_./".contains(c));
    match kind {
        Kind::List | Kind::Command => v.to_string(),
        Kind::Text | Kind::Password if !(plain && v.starts_with('*')) => {
            format!("'{}'", v.replace('\'', "''"))
        }
        _ if plain => v.to_string(),
        _ => format!("'{}'", v.replace('\'', "''")),
    }
}

/// Build a command from prompt values (keyword -> text as typed). Values
/// left empty or equal to the default are left out.
pub fn build(def: &CommandDef, values: &BTreeMap<String, String>) -> String {
    let mut out = def.name.to_string();
    for pd in def.params {
        let v = values.get(pd.keyword).map(|s| s.trim()).unwrap_or("");
        if v.is_empty() {
            continue;
        }
        if let Some(d) = pd.default {
            if v.eq_ignore_ascii_case(d) && pd.kind != Kind::Password {
                continue;
            }
        }
        out.push_str(&format!(" {}({})", pd.keyword, cl_value(pd.kind, v)));
    }
    out
}

/// The text of a parameter's values as typed into a prompt field.
pub fn prompt_text(kind: Kind, vals: &[Value]) -> String {
    match kind {
        Kind::List => vals
            .iter()
            .map(|v| match v {
                Value::Quoted(q) => format!("'{}'", q.replace('\'', "''")),
                Value::Word(w) => w.clone(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => vals
            .first()
            .map(|v| v.text().to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_and_positional_forms_are_equivalent() {
        let a = parse("crtlib lib(mylib) text('My ''own'' library')").unwrap();
        let b = parse("CRTLIB MYLIB *PROD 'My ''own'' library'").unwrap();
        for c in [&a, &b] {
            assert_eq!(c.def.name, "CRTLIB");
            assert_eq!(c.get("LIB"), "MYLIB");
            assert_eq!(
                c.get("TEXT"),
                "My 'own' library",
                "quoted text keeps case and doubles apostrophes"
            );
            assert_eq!(c.get("TYPE"), "*PROD");
        }
    }

    #[test]
    fn defaults_and_lists() {
        assert_eq!(parse("dsplib").unwrap().get("LIB"), "*LIBL");
        let c = parse("CALL PGM(custlist) PARM('3' abc)").unwrap();
        assert_eq!(c.get("PGM"), "CUSTLIST");
        assert_eq!(c.list("PARM"), vec!["3", "ABC"]);
    }

    #[test]
    fn partial_parse_and_build_round_trip() {
        // F4 on a partly typed command keeps what was typed.
        let (def, vals) = parse_partial("crtlib mylib").unwrap();
        assert_eq!(def.name, "CRTLIB");
        assert_eq!(prompt_text(Kind::Name, &vals["LIB"]), "MYLIB");
        assert!(!vals.contains_key("TEXT"));
        // Back from the prompt: defaults are left out, text is quoted.
        let mut v = BTreeMap::new();
        v.insert("LIB".to_string(), "MYLIB".to_string());
        v.insert("TYPE".to_string(), "*PROD".to_string());
        v.insert("TEXT".to_string(), "Bob's stuff".to_string());
        let line = build(def, &v);
        assert_eq!(line, "CRTLIB LIB(MYLIB) TEXT('Bob''s stuff')");
        let c = parse(&line).unwrap();
        assert_eq!(c.get("TEXT"), "Bob's stuff");
        // Lists keep their quoting.
        let (def, vals) = parse_partial("call custlist parm('3' x)").unwrap();
        let parm = prompt_text(Kind::List, &vals["PARM"]);
        assert_eq!(parm, "'3' X");
        let mut v = BTreeMap::new();
        v.insert("PGM".to_string(), "CUSTLIST".to_string());
        v.insert("PARM".to_string(), parm);
        assert_eq!(parse(&build(def, &v)).unwrap().list("PARM"), vec!["3", "X"]);
    }

    #[test]
    fn element_lists_inside_a_value() {
        // DTAARA(name (start length)), as in CHGDTAARA's syntax diagram.
        let c = parse("CHGDTAARA DTAARA(QGPL/CO (6 5)) VALUE('Ltd')").unwrap();
        assert_eq!(c.list("DTAARA"), ["QGPL/CO", "(6 5)"]);
        assert_eq!(c.get("VALUE"), "Ltd");
        let c = parse("crtdtaara nxt *dec len(7 0) value(1004)").unwrap();
        assert_eq!(c.list("LEN"), ["7", "0"]);
        assert!(parse("CHGDTAARA DTAARA(CO (1 (2))) VALUE(X)").is_err());
    }

    #[test]
    fn call_parm_values_with_a_type() {
        // CL Reference, CALL: a parameter can be written (value (type
        // length)), e.g. PARM(('abc' (*CHAR 128))). Nowhere else.
        let c = parse("CALL NWAPP/NWBUILD PARM(('/home/me' (*CHAR 128)) 'NWAPP')").unwrap();
        assert_eq!(c.list("PARM"), ["('/home/me' (*CHAR 128))", "NWAPP"]);
        assert!(parse("CALL X PARM(('a' (*CHAR (1))))").is_err());
    }

    #[test]
    fn command_values_keep_their_parentheses() {
        // SBMJOB CMD takes a whole command, nested lists and quotes included.
        let c = parse("SBMJOB CMD(CHGDTAARA DTAARA(X (1 2)) VALUE('it''s')) JOB(nightly)").unwrap();
        assert_eq!(c.get("CMD"), "CHGDTAARA DTAARA(X (1 2)) VALUE('it''s')");
        assert_eq!(c.get("JOB"), "NIGHTLY");
        let c = parse("sbmjob cmd(call pgm(custrpt) parm('L'))").unwrap();
        assert_eq!(c.get("CMD"), "call pgm(custrpt) parm('L')");
        assert!(parse("SBMJOB CMD(CALL PGM(X)").is_err());
    }

    #[test]
    fn qualified_and_generic_names() {
        assert_eq!(parse("wrkobj qgpl/cust*").unwrap().get("OBJ"), "QGPL/CUST*");
        assert_eq!(parse("wrkobj *all").unwrap().get("OBJ"), "*ALL");
        assert_eq!(
            parse("wrkobj *libl/custmast *file").unwrap().get("OBJTYPE"),
            "*FILE"
        );
        assert!(parse("wrkobj 1bad/x").is_err());
        assert!(
            parse("dsppfm qgpl/cust*").is_err(),
            "DSPPFM takes no generic name"
        );
        // Keyword-only parameters cannot be given by position.
        assert!(parse("crtusrprf bob secret *yes").is_err());
        assert_eq!(
            parse("crtusrprf bob secret pwdexp(*yes)")
                .unwrap()
                .get("PWDEXP"),
            "*YES"
        );
    }

    #[test]
    fn every_command_is_in_exactly_one_menu_group() {
        for c in COMMANDS {
            let n = GROUPS
                .iter()
                .filter(|g| g.commands.contains(&c.name))
                .count();
            assert_eq!(n, 1, "{} is in {n} groups", c.name);
        }
        for g in GROUPS {
            for name in g.commands {
                assert!(find(name).is_some(), "group lists unknown command {name}");
            }
        }
    }

    #[test]
    fn required_parameters_are_on_the_first_prompt_screen() {
        // The prompt shows parameters on lines 5-21 (qualified names take
        // two), more on the pages after; the required ones come first.
        for c in COMMANDS {
            let mut lines = 0;
            for p in c.params {
                lines += if p.kind == Kind::Qualified { 2 } else { 1 };
                assert!(
                    !p.required || lines <= 17,
                    "{}: required parameter {} is not on the first screen",
                    c.name,
                    p.keyword
                );
            }
        }
    }

    #[test]
    fn errors() {
        let e = |s: &str| parse(s).unwrap_err().to_string();
        assert!(e("NOSUCH").starts_with("CPD0030"), "{}", e("NOSUCH"));
        assert!(e("CRTLIB").contains("required parameter LIB"));
        assert!(e("CRTLIB LIB(A) FOO(B)").contains("Keyword FOO not valid"));
        assert!(e("CRTLIB LIB(A B)").contains("Too many values"));
        assert!(e("CRTLIB LIB(1ABC)").contains("not a valid"));
        assert!(e("CRTLIB TEXT('x").contains("apostrophe"));
        assert!(e("DSPLIB A B").contains("Too many positional"));
    }
}
