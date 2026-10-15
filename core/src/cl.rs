//! CL command parsing, driven by a table of command definitions.
//!
//! Syntax (from the public CL reference): a command name followed by
//! parameters, each either positional or `KEYWORD(value ...)`. Values are
//! names (folded to upper case), special values like `*LIBL`, qualified
//! names `LIB/OBJ`, numbers, or quoted strings `'it''s'` (case kept).
//!
//! Each command is declared once, as data ([`CommandDef`]); the same table
//! will drive prompting (F4) and help later.

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
    }
}

pub static COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "CRTLIB",
        description: "Create Library",
        params: &[
            p("LIB", Kind::Name, true, None, "Library"),
            p("TYPE", Kind::Name, false, Some("*PROD"), "Library type"),
            p("TEXT", Kind::Text, false, Some(""), "Text 'description'"),
        ],
    },
    CommandDef {
        name: "DSPLIB",
        description: "Display Library",
        params: &[p("LIB", Kind::Name, false, Some("*LIBL"), "Library")],
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
            p("LIB", Kind::Name, true, None, "Library"),
            p(
                "POSITION",
                Kind::Name,
                false,
                Some("*FIRST"),
                "Library list position",
            ),
        ],
    },
    CommandDef {
        name: "CALL",
        description: "Call Program",
        params: &[
            p("PGM", Kind::Name, true, None, "Program"),
            p("PARM", Kind::List, false, None, "Parameters"),
        ],
    },
    CommandDef {
        name: "SIGNOFF",
        description: "Sign Off",
        params: &[p("LOG", Kind::Name, false, Some("*NOLIST"), "Job log")],
    },
];

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

pub fn parse(line: &str) -> Result<Command> {
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
        if pd.kind == Kind::Name {
            if let Some(v) = params.get(pd.keyword).and_then(|v| v.first()) {
                let t = v.text();
                if !t.starts_with('*') && !t.is_empty() {
                    crate::catalog::validate_name(&pd.prompt.to_ascii_lowercase(), t)
                        .map_err(err)?;
                }
            }
        }
    }
    Ok(Command { def, params })
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
