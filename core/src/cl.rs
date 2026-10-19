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
        params: &[p("DTAARA", Kind::Qualified, true, None, "Data area").ch("Name")],
    },
    CommandDef {
        name: "DLTDTAARA",
        description: "Delete Data Area",
        params: &[p("DTAARA", Kind::Qualified, true, None, "Data area").ch("Name")],
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
        commands: &["DSPPFM"],
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
        commands: &["STRSQL", "RUNSQL"],
    },
    Group {
        item: "Program commands",
        title: "Program Commands",
        commands: &["CALL"],
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
            "DSPFD",
            "CRTPF",
            "CRTLF",
            "CRTDSPF",
            "CRTBNDCL",
            "CRTCLPGM",
            "CRTBNDRPG",
            "CRTSQLRPGI",
        ],
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
];

/// Library part of a qualified name: special values allowed.
const LIBRARY_SPECIALS: &[&str] = &["*LIBL", "*CURLIB", "*ALL", "*USRLIBL", "*ALLUSR"];

pub fn find(name: &str) -> Option<&'static CommandDef> {
    COMMANDS.iter().find(|c| c.name.eq_ignore_ascii_case(name))
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
                            let mut inner = Vec::new();
                            loop {
                                match it.next() {
                                    Some(Tok::Close) => break,
                                    Some(Tok::Word(v)) => inner.push(v),
                                    Some(Tok::Quoted(v)) => {
                                        inner.push(format!("'{}'", v.replace('\'', "''")))
                                    }
                                    Some(Tok::Open) => {
                                        return Err(err(
                                            "Lists nested more than two deep are not supported.",
                                        ))
                                    }
                                    None => {
                                        return Err(err(format!(
                                            "A closing parenthesis is missing after {w}."
                                        )))
                                    }
                                }
                            }
                            vals.push(Value::Word(format!("({})", inner.join(" "))));
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
    fn every_command_fits_one_prompt_screen() {
        // The prompt shows parameters on lines 5-21; qualified names take two.
        for c in COMMANDS {
            let lines: usize = c
                .params
                .iter()
                .map(|p| if p.kind == Kind::Qualified { 2 } else { 1 })
                .sum();
            assert!(
                lines <= 17,
                "{} has too many parameters for one screen",
                c.name
            );
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
