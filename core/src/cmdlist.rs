//! CRTCMD's listing: the command's attributes, the command definition
//! source by sequence number with the messages about each statement, a
//! cross reference of the keywords, the final messages and their summary.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-26,
//! tools/golden/verify1.txt v_crtcmd and qsh.txt q_crtcmd). The product
//! line and the compiler's name are as4tainr's (report.rs); the message
//! texts are as4tainr's own.

use crate::report::{at, Stamp};

/// A message of the listing: ID, severity, the sequence number of the
/// statement it is about (0: none), its text, and whether it is also
/// printed under the statement in the source.
#[derive(Debug, Clone)]
pub struct Msg {
    pub id: String,
    pub sev: u32,
    pub seq: u32,
    pub text: String,
    pub inline: bool,
}

/// The command's attributes the first page shows.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub name: String,
    pub library: String,
    pub pgm: (String, String),
    pub srcfile: (String, String),
    pub member: String,
    pub changed: Option<Stamp>,
    pub vldckr: String,
    pub mode: Vec<String>,
    pub allow: Vec<String>,
    pub maxpos: String,
    pub text: String,
    pub replace: String,
    pub lines: Vec<String>,
}

/// The ALLOW(*ALL) environments, in the order the listing shows them.
pub const ALL_ENVIRONMENTS: &[&str] = &[
    "*IREXX",
    "*BREXX",
    "*BPGM",
    "*IPGM",
    "*EXEC",
    "*INTERACT",
    "*BATCH",
    "*BMOD",
    "*IMOD",
];

/// The parameters of CRTCMD that a CMD statement can override, in
/// CRTCMD's order: the override messages come in the reverse order
/// (v_crtcmd: TEXT, ALLOW, VLDCKR for a statement giving VLDCKR, ALLOW
/// and TEXT).
pub const OVERRIDABLE: &[&str] = &["VLDCKR", "MODE", "ALLOW", "ALWLMTUSR", "MAXPOS", "TEXT"];

/// Lines on a page, the header included.
const PAGE: usize = 60;

/// A statement of the source: its first and last line (0-based), its
/// command word and its text.
pub struct Statement {
    pub first: usize,
    pub last: usize,
    pub command: String,
    pub text: String,
}

/// The statements of command definition source: a line ending in + or -
/// goes on in the next one; a label (NAME:) may come first.
pub fn statements(lines: &[String]) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() || lines[i].trim_start().starts_with("/*") {
            i += 1;
            continue;
        }
        let first = i;
        let mut text = String::new();
        loop {
            let l = lines[i].trim_end();
            let cont = l.ends_with('+') || l.ends_with('-');
            text.push_str(if cont { &l[..l.len() - 1] } else { l });
            text.push(' ');
            if !cont || i + 1 >= lines.len() {
                break;
            }
            i += 1;
        }
        let mut words = text.split_whitespace();
        let mut command = words.next().unwrap_or("").to_ascii_uppercase();
        if command.ends_with(':') {
            command = words.next().unwrap_or("").to_ascii_uppercase();
        }
        out.push(Statement {
            first,
            last: i,
            command,
            text,
        });
        i += 1;
    }
    out
}

/// The sequence number a source line is listed with (records 1.00, 2.00
/// ... are 100, 200 ...).
pub fn seq(line: usize) -> u32 {
    (line as u32 + 1) * 100
}

/// The listing's lines. `created`: the completion message's text.
pub fn listing(input: &Input, msgs: &[Msg], created: &str, stamp: &Stamp) -> Vec<String> {
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (41, "Command Definition"),
            (67, &format!("{}/{}", input.library, input.name)),
            (89, &crate::report::system_name()),
            (96, &format!("{} {}", stamp.date, stamp.time)),
            (122, &format!("Page {page:>6}")),
        ])
    };
    let label = |indent: usize, text: &str, value: &str| {
        // The colon in column 48; the value two blanks after it, four for
        // a second-level label (as captured).
        let l = crate::commands::dsp_label(text, 46 - indent);
        let gap = if indent > 2 { "     " } else { "   " };
        format!("{}{l}{gap}{value}", " ".repeat(indent - 1))
            .trim_end()
            .to_string()
    };
    let more = |value: &str| format!("{:51}{value}", "");
    let mut first = vec![
        label(2, "Command name", &input.name),
        label(4, "Library", &input.library),
        label(2, "Command processing program", &input.pgm.1),
        label(4, "Library", &input.pgm.0),
        label(2, "Source file", &input.srcfile.1),
        label(4, "Library", &input.srcfile.0),
        label(
            2,
            "Source file member",
            &match &input.changed {
                Some(c) => format!("{:<13}{}  {}", input.member, c.date, c.time),
                None => input.member.clone(),
            },
        ),
        label(2, "Source printing options", "*NOEVENTF"),
        label(2, "Validity checker program", &input.vldckr),
    ];
    let list = |first: &mut Vec<String>, text: &str, values: &[String]| {
        for (k, v) in values.iter().enumerate() {
            if k == 0 {
                first.push(label(2, text, v));
            } else {
                first.push(more(v));
            }
        }
    };
    list(&mut first, "Mode in which valid", &input.mode);
    list(&mut first, "Environment allowed", &input.allow);
    for (t, v) in [
        ("Allow limited user", "*NO"),
        ("Max positional parameters", input.maxpos.as_str()),
        ("Prompt file", "*NONE"),
    ] {
        first.push(label(2, t, v));
    }
    first.push(label(2, "Message file", "QCPFMSG"));
    first.push(label(4, "Library", "*LIBL"));
    for (t, v) in [
        ("Authority", "*LIBCRTAUT"),
        ("Replace command", input.replace.as_str()),
        ("Enable graphical user interface", "*NO"),
        ("Threadsafe", "*NO"),
        ("Multithreaded job action", "*SYSVAL"),
        (
            "Text",
            if input.text.is_empty() {
                "*NONE"
            } else {
                input.text.as_str()
            },
        ),
        ("Help panel group", "*NONE"),
        ("Help identifier", "*NONE"),
        ("Help search index", "*NONE"),
        ("Current library", "*NOCHG"),
        ("Product library", "*NOCHG"),
        ("Prompt override program", "*NONE"),
        ("Compiler", "as4tainr command definition compiler"),
    ] {
        first.push(label(2, t, v));
    }
    first.push(format!("{:37}Command Definition Source", ""));
    first.push(" SEQNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+.  DATE".into());
    let stmts = statements(&input.lines);
    for (i, l) in input.lines.iter().enumerate() {
        let starts = stmts.iter().any(|s| s.first == i);
        first.push(
            format!(
                "{:>7}{}  {}",
                seq(i),
                if starts { "-" } else { " " },
                l.trim_end()
            )
            .trim_end()
            .to_string(),
        );
        // The messages about a statement, under its last line.
        if let Some(s) = stmts.iter().find(|s| s.last == i) {
            for m in msgs.iter().filter(|m| m.inline && m.seq == seq(s.first)) {
                first.push(format!("* {:<7}  {:02}  {}", m.id, m.sev, m.text));
            }
        }
    }
    first.push(format!(
        "{:41}* * * * *   E N D   O F   S  O U R C E   * * * * *",
        ""
    ));
    let mut xref = vec![
        format!("{:42}Cross Reference", ""),
        " Defined Keywords".to_string(),
        " Keyword              Number          Defined      References".to_string(),
    ];
    let mut n = 0;
    for s in stmts.iter().filter(|s| s.command == "PARM") {
        n += 1;
        let kwd = keyword(&s.text).unwrap_or_default();
        xref.push(format!(
            " {kwd:<20}{:>6}{:>18}",
            format!("{n:03}"),
            seq(s.first)
        ));
    }
    xref.push(format!(
        "{:31}* * * * *   E N D   O F   C R O S S   R E F E R E N C E   * * * * *",
        ""
    ));
    let mut fin = vec![
        format!("{:42}Final Messages", ""),
        " Message               Sequence".to_string(),
        " ID                     Number          Sev        Text".to_string(),
    ];
    let row = |id: &str, seq: &str, sev: u32, text: &str| {
        format!(
            "* {id:<7}{seq:>21}{:>13}        {text}",
            format!("{sev:02}")
        )
    };
    for m in msgs {
        let s = if m.seq == 0 {
            String::new()
        } else {
            m.seq.to_string()
        };
        fin.push(row(&m.id, &s, m.sev, &m.text));
    }
    let errors = msgs.iter().filter(|m| m.sev >= 30).count();
    fin.push(format!("{:42}Message Summary", ""));
    fin.push(" Total         Info        Error".to_string());
    fin.push(format!(
        "{:>6}{:>14}{:>12}",
        msgs.len(),
        msgs.len() - errors,
        errors
    ));
    fin.push(row("CPC0202", "", 0, created));
    fin.push(format!(
        "{:35}* * * * *   E N D   O F   C O M P I L A T I O N   * * * * *",
        ""
    ));
    let mut out = Vec::new();
    let mut page = 0;
    for section in [first, xref, fin] {
        for chunk in section.chunks(PAGE - 1) {
            page += 1;
            out.push(header(page));
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

/// KWD(...) of a PARM statement.
pub fn keyword(text: &str) -> Option<String> {
    let up = text.to_ascii_uppercase();
    let i = up.find("KWD(")?;
    let rest = &up[i + 4..];
    Some(rest[..rest.find(')')?].trim().to_string())
}

/// Whether a statement gives a keyword (`KW(`).
pub fn gives(text: &str, kw: &str) -> bool {
    let up = text.to_ascii_uppercase();
    up.split(|c: char| c.is_whitespace())
        .any(|w| w.starts_with(&format!("{kw}(")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(lines: &[&str]) -> Input {
        Input {
            name: "GLDVCMD".into(),
            library: "AS4TRAINR1".into(),
            pgm: ("AS4TRAINR1".into(), "GLDVCPP".into()),
            srcfile: ("AS4TRAINR1".into(), "GLDVSRC".into()),
            member: "GLDVCMD".into(),
            changed: Some(Stamp {
                date: "26-10-26".into(),
                time: "10:05:13".into(),
                zone: String::new(),
            }),
            vldckr: "*NONE".into(),
            mode: vec!["*PROD".into(), "*DEBUG".into(), "*SERVICE".into()],
            allow: ALL_ENVIRONMENTS.iter().map(|s| s.to_string()).collect(),
            maxpos: "*NOMAX".into(),
            text: String::new(),
            replace: "*YES".into(),
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    // Expected lines: PUB400 (IBM i 7.5) CRTCMD listing, 2026-10-26,
    // tools/golden/verify1.txt v_crtcmd (the product line, the compiler's
    // name and the message texts are as4tainr's).
    #[test]
    fn listing_layout() {
        let src = [
            "             CMD        PROMPT('Golden test') VLDCKR(GLDVCHK) +",
            "                          ALLOW(*BPGM *IPGM) TEXT(*CMDPMT)",
            "             PARM       KWD(IN) TYPE(*CHAR) LEN(10) MIN(1)",
            "             PARM       KWD(OUT) TYPE(*CHAR) LEN(10) RTNVAL(*YES) MIN(1)",
        ];
        let msgs = vec![Msg {
            id: "CPD079A".into(),
            sev: 10,
            seq: 100,
            text: "x".into(),
            inline: true,
        }];
        let stamp = Stamp {
            date: "26-10-26".into(),
            time: "10:05:13".into(),
            zone: String::new(),
        };
        let l = listing(&input(&src), &msgs, "made", &stamp);
        assert!(l[0].starts_with(
            " AS4T001 V0R1M0  261021                 Command Definition        AS4TRAINR1/GLDVCMD"
        ));
        assert!(l[0].ends_with("Page      1"), "{}", l[0]);
        assert_eq!(
            l[1],
            " Command name  . . . . . . . . . . . . . . . . :   GLDVCMD"
        );
        assert_eq!(
            l[2],
            "   Library . . . . . . . . . . . . . . . . . . :     AS4TRAINR1"
        );
        assert_eq!(
            l[3],
            " Command processing program  . . . . . . . . . :   GLDVCPP"
        );
        assert_eq!(
            l[7],
            " Source file member  . . . . . . . . . . . . . :   GLDVCMD      26-10-26  10:05:13"
        );
        assert_eq!(
            l[8],
            " Source printing options . . . . . . . . . . . :   *NOEVENTF"
        );
        assert_eq!(
            l[10],
            " Mode in which valid . . . . . . . . . . . . . :   *PROD"
        );
        assert_eq!(
            l[11],
            "                                                   *DEBUG"
        );
        let i = l
            .iter()
            .position(|x| x.contains("Command Definition Source"))
            .unwrap();
        assert_eq!(
            l[i],
            "                                     Command Definition Source"
        );
        assert_eq!(
            l[i + 2],
            "    100-               CMD        PROMPT('Golden test') VLDCKR(GLDVCHK) +"
        );
        assert_eq!(
            l[i + 3],
            "    200                             ALLOW(*BPGM *IPGM) TEXT(*CMDPMT)"
        );
        assert_eq!(l[i + 4], "* CPD079A  10  x");
        assert_eq!(
            l[i + 5],
            "    300-               PARM       KWD(IN) TYPE(*CHAR) LEN(10) MIN(1)"
        );
        assert_eq!(
            l[i + 7],
            "                                         * * * * *   E N D   O F   S  O U R C E   * * * * *"
        );
        assert!(l[i + 8].ends_with("Page      2"));
        assert_eq!(
            l[i + 11],
            " Keyword              Number          Defined      References"
        );
        assert_eq!(l[i + 12], " IN                     001               300");
        assert_eq!(l[i + 13], " OUT                    002               400");
        let f = l.iter().position(|x| x.contains("Final Messages")).unwrap();
        assert_eq!(
            l[f + 3],
            "* CPD079A                  100           10        x"
        );
        assert_eq!(l[f + 5], " Total         Info        Error");
        assert_eq!(l[f + 6], "     1             1           0");
        assert_eq!(
            l[f + 7],
            "* CPC0202                                00        made"
        );
        assert_eq!(
            l[f + 8],
            "                                   * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *"
        );
    }
}
