//! Menus: GO, CRTMNU, DLTMNU, and the menus supplied with the system.
//!
//! From IBM's public CL reference: `GO MENU(name)` shows a menu. A menu
//! lists numbered options and has a "Selection or command" line, where
//! the operator types an option number or any command. Each option runs a
//! command; a `?` in front of the command name prompts it first. Option 90
//! signs off.
//!
//! The system's menus (MAIN and the menus it leads to) are built in and
//! belong to QSYS. Their titles, options and texts are as4tainr's own: they
//! lead to the commands as4tainr has, not to the menus of IBM i.
//!
//! User menus are made with CRTMNU. Only TYPE(*PGM) is supported: the menu
//! is a program, called when the menu is shown (it draws its own display);
//! when it returns, the menu is done. TYPE(*DSPF) (a display file with
//! message-file commands) and TYPE(*UIM) are not supported yet.

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.menus (
    library   text NOT NULL,
    name      text NOT NULL,
    menu_type text NOT NULL,
    program   text NOT NULL,
    PRIMARY KEY (library, name)
);
"#;

/// One option of a built-in menu: its number, its text, and the command
/// it runs (`?` first: prompt the command).
#[derive(Debug, Clone, Copy)]
pub struct MenuOption {
    pub number: u8,
    pub text: &'static str,
    pub command: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinMenu {
    pub name: &'static str,
    pub title: &'static str,
    pub options: &'static [MenuOption],
}

const fn o(number: u8, text: &'static str, command: &'static str) -> MenuOption {
    MenuOption {
        number,
        text,
        command,
    }
}

/// Option 90 on every system menu.
const SIGN_OFF: MenuOption = o(90, "Sign off", "SIGNOFF");

pub const BUILTIN: &[BuiltinMenu] = &[
    BuiltinMenu {
        name: "MAIN",
        title: "as4tainr Main Menu",
        options: &[
            o(1, "User tasks", "GO USER"),
            o(2, "Jobs and printed output", "GO JOBS"),
            o(3, "Files, libraries and objects", "GO FILES"),
            o(4, "Programming", "GO PROGRAM"),
            o(5, "Security", "GO SECURITY"),
            o(6, "Display a menu", "?GO"),
            o(7, "Command entry", "CALL QCMD"),
            SIGN_OFF,
        ],
    },
    BuiltinMenu {
        name: "USER",
        title: "User Tasks",
        options: &[
            o(1, "Display your messages", "DSPMSG"),
            o(2, "Send a message", "?SNDMSG"),
            o(3, "Work with your printed output", "WRKSPLF"),
            o(4, "Work with your submitted jobs", "WRKSBMJOB"),
            o(5, "Display your job log", "DSPJOBLOG"),
            o(6, "Change your password", "CHGPWD"),
            o(7, "Display your library list", "DSPLIBL"),
            SIGN_OFF,
        ],
    },
    BuiltinMenu {
        name: "JOBS",
        title: "Jobs and Printed Output",
        options: &[
            o(1, "Work with active jobs", "WRKACTJOB"),
            o(2, "Work with submitted jobs", "WRKSBMJOB"),
            o(3, "Submit a job", "?SBMJOB"),
            o(4, "End a job", "?ENDJOB"),
            o(5, "Work with spooled files", "WRKSPLF"),
            o(6, "Display your job log", "DSPJOBLOG"),
            SIGN_OFF,
        ],
    },
    BuiltinMenu {
        name: "FILES",
        title: "Files, Libraries and Objects",
        options: &[
            o(1, "Display your library list", "DSPLIBL"),
            o(2, "Display a library", "?DSPLIB"),
            o(3, "Create a library", "?CRTLIB"),
            o(4, "Work with objects", "?WRKOBJ"),
            o(5, "Display the records of a file", "?DSPPFM"),
            o(6, "Display a data area", "?DSPDTAARA"),
            o(7, "Run SQL statements", "STRSQL"),
            SIGN_OFF,
        ],
    },
    BuiltinMenu {
        name: "PROGRAM",
        title: "Programming",
        options: &[
            o(1, "Command entry", "CALL QCMD"),
            o(2, "Call a program", "?CALL"),
            o(3, "Run SQL statements", "STRSQL"),
            o(4, "Create a menu", "?CRTMNU"),
            o(5, "Create a data area", "?CRTDTAARA"),
            SIGN_OFF,
        ],
    },
    BuiltinMenu {
        name: "SECURITY",
        title: "Security",
        options: &[
            o(1, "Create a user profile", "?CRTUSRPRF"),
            o(2, "Change a user profile", "?CHGUSRPRF"),
            o(3, "Display a user profile", "?DSPUSRPRF"),
            o(4, "Delete a user profile", "?DLTUSRPRF"),
            o(5, "Display object authority", "?DSPOBJAUT"),
            o(6, "Grant object authority", "?GRTOBJAUT"),
            o(7, "Revoke object authority", "?RVKOBJAUT"),
            o(8, "Change object owner", "?CHGOBJOWN"),
            SIGN_OFF,
        ],
    },
];

/// A menu found by GO.
#[derive(Debug, Clone)]
pub enum Menu {
    Builtin(&'static BuiltinMenu),
    /// CRTMNU TYPE(*PGM): call this program (LIB/NAME).
    Program {
        library: String,
        name: String,
        program: String,
    },
}

impl Menu {
    pub fn name(&self) -> &str {
        match self {
            Menu::Builtin(b) => b.name,
            Menu::Program { name, .. } => name,
        }
    }
}

pub fn builtin(name: &str) -> Option<&'static BuiltinMenu> {
    BUILTIN.iter().find(|m| m.name == name)
}

/// Find a menu: `NAME` (library list), `*LIBL/NAME`, `LIB/NAME`. The
/// system menus are in QSYS, which is first on every library list.
pub async fn find(client: &Client, qualified: &str, libl: &[String]) -> Result<Menu> {
    let qualified = qualified.trim().to_ascii_uppercase();
    let (lib, name) = match qualified.split_once('/') {
        Some((l, n)) => (l.to_string(), n.to_string()),
        None => ("*LIBL".to_string(), qualified.clone()),
    };
    let name = validate_name("menu", &name)?;
    if lib == "*LIBL" || lib == "QSYS" {
        if let Some(b) = builtin(&name) {
            return Ok(Menu::Builtin(b));
        }
    }
    let libs: Vec<String> = if lib == "*LIBL" {
        libl.to_vec()
    } else {
        vec![lib.clone()]
    };
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT program FROM as4tainr.menus WHERE library=$1 AND name=$2",
                &[l, &name],
            )
            .await?
        {
            return Ok(Menu::Program {
                library: l.clone(),
                name: name.clone(),
                program: r.get(0),
            });
        }
    }
    bail!("Menu {name} in library {lib} not found.");
}

/// CRTMNU TYPE(*PGM). `program` is LIB/NAME.
pub async fn create(
    client: &Client,
    library: &str,
    name: &str,
    program: &str,
    text: &str,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("menu", name)?;
    if lib == "QSYS" {
        bail!("Menus cannot be created in QSYS.");
    }
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF2110: Library {lib} not found.");
    }
    let r = client
        .execute(
            "WITH o AS (
                INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                VALUES ($1, $2, '*MENU', 'PGM', $4) ON CONFLICT DO NOTHING RETURNING 1)
             INSERT INTO as4tainr.menus SELECT $1, $2, '*PGM', $3 FROM o",
            &[&lib, &name, &program, &text],
        )
        .await?;
    if r == 0 {
        bail!("Menu {name} already exists in library {lib}.");
    }
    Ok(())
}

/// DLTMNU. Returns false if there was no such menu.
pub async fn delete(client: &Client, library: &str, name: &str) -> Result<bool> {
    let n = client
        .execute(
            "WITH m AS (DELETE FROM as4tainr.menus WHERE library=$1 AND name=$2 RETURNING 1)
             DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*MENU'
                AND EXISTS (SELECT 1 FROM m)",
            &[&library, &name],
        )
        .await?;
    Ok(n > 0)
}

/// The library of a user menu on the library list (for DLTMNU *LIBL).
pub async fn library_of(client: &Client, name: &str, libl: &[String]) -> Result<Option<String>> {
    for l in libl {
        if client
            .query_opt(
                "SELECT 1 FROM as4tainr.menus WHERE library=$1 AND name=$2",
                &[l, &name],
            )
            .await?
            .is_some()
        {
            return Ok(Some(l.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_menus_are_consistent() {
        for m in BUILTIN {
            // Option 90 signs off, on every menu (IBM i convention).
            assert!(m
                .options
                .iter()
                .any(|o| o.number == 90 && o.command == "SIGNOFF"));
            let mut numbers: Vec<u8> = m.options.iter().map(|o| o.number).collect();
            numbers.dedup();
            assert_eq!(
                numbers.len(),
                m.options.len(),
                "{}: duplicate option",
                m.name
            );
            // Every option's command is a command as4tainr knows, and every
            // GO leads to a menu that exists.
            for o in m.options {
                let cmd = o.command.trim_start_matches('?');
                let first = cmd.split_whitespace().next().unwrap();
                assert!(
                    crate::cl::find(first).is_some(),
                    "{}: {}",
                    m.name,
                    o.command
                );
                if let Some(target) = cmd.strip_prefix("GO ") {
                    assert!(builtin(target).is_some(), "{}: {}", m.name, o.command);
                }
            }
        }
        assert!(builtin("MAIN").is_some());
    }
}
