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
            p("PGM", Kind::Name, true, None, "Program").ch("Name"),
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
        ],
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
            p("OBJTYPE", Kind::Name, false, Some("*ALL"), "Object type").ch("*ALL, *LIB, *FILE"),
        ],
    },
    CommandDef {
        name: "DSPPFM",
        description: "Display Physical File Member",
        params: &[p("FILE", Kind::Qualified, true, None, "File").ch("Name")],
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
        commands: &["WRKOBJ"],
    },
    Group {
        item: "File commands",
        title: "File Commands",
        commands: &["DSPPFM"],
    },
    Group {
        item: "User profile commands",
        title: "User Profile Commands",
        commands: &["CRTUSRPRF", "DSPUSRPRF", "DLTUSRPRF", "CHGPWD"],
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

fn err(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("CPF0006: Errors occurred in command. {msg}")
}

/// Parse a command without checking for required parameters or filling in
/// defaults: what F4 prompting starts from.
pub fn parse_partial(
    line: &str,
) -> Result<(&'static CommandDef, BTreeMap<&'static str, Vec<Value>>)> {
    let toks = tokenize(line.trim())?;
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
                        Some(Tok::Open) => return Err(err("Nested lists are not supported yet.")),
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
    if obj == "*ALL" && pd.choices.contains("*ALL") {
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
        Kind::List => v.to_string(),
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
