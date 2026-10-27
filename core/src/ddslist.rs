//! The DDS listing CRTPF and CRTLF print (QSYSPRT): the file's
//! attributes, the source by sequence number with markers for the
//! messages about a line, the expanded source (physical file fields with
//! their attributes, lengths and buffer positions), the messages, and the
//! message summary with the command's own messages.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-27,
//! tools/golden/verify5.txt z_crtpf, z_crtpf_dup, z_crtlf, z_crtpf_bad).
//! The product line and the compiler's name are as4tainr's (report.rs);
//! the message texts are as4tainr's own.

use crate::report::{at, Stamp};

/// A message about the DDS: its ID (none where no capture shows one),
/// severity, the source line (1-based; 0: none) and the position in it
/// the marker points at.
#[derive(Debug, Clone)]
pub struct Msg {
    pub id: Option<String>,
    pub sev: u32,
    pub line: usize,
    pub pos: usize,
    pub text: String,
}

/// An error found in DDS source, with what the listing shows about it.
#[derive(Debug, Clone)]
pub struct DdsError(pub Msg);

impl std::fmt::Display for DdsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.0.line, self.0.text)
    }
}

impl std::error::Error for DdsError {}

/// The message a DDS parse error makes: a `DdsError` as it is, otherwise
/// the line from "line N: " and no ID, severity 30.
pub fn message_of(e: &anyhow::Error) -> Msg {
    if let Some(d) = e.downcast_ref::<DdsError>() {
        return d.0.clone();
    }
    let s = format!("{e:#}");
    let (line, text) = match s
        .strip_prefix("line ")
        .and_then(|r| r.split_once(": "))
        .and_then(|(n, t)| Some((n.parse::<usize>().ok()?, t.to_string())))
    {
        Some((n, t)) => (n, t),
        None => (0, s.clone()),
    };
    Msg {
        id: None,
        sev: 30,
        line,
        pos: 0,
        text,
    }
}

/// What the first page shows.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub name: String,
    pub library: String,
    /// "Physical" or "Logical".
    pub attribute: String,
    pub srcfile: (String, String),
    pub member: String,
    pub changed: Option<Stamp>,
    pub options: Vec<String>,
    pub genlvl: String,
    pub flag: String,
    pub filetype: String,
    pub aut: String,
    pub replace: String,
    pub text: String,
    pub lines: Vec<String>,
    /// Whether the expanded source shows physical file fields with their
    /// attributes (CRTPF).
    pub physical: bool,
}

const PAGE: usize = 60;

/// The sequence number a source line is listed with (1-based line n:
/// n * 100, as records 1.00, 2.00 ... are).
pub fn seq(line: usize) -> usize {
    line * 100
}

fn col(line: &str, from: usize, to: usize) -> &str {
    let start = from - 1;
    if line.len() <= start {
        return "";
    }
    line.get(start..to.min(line.len())).unwrap_or("").trim()
}

/// A physical file field line on its own (a record format with that one
/// field), or None when it is not a field or has an error.
fn field_of(line: &str) -> Option<crate::dds::FieldDef> {
    let src = format!("{:5}A{:10}R X\n{line}\n", "", "");
    crate::dds::parse_pf("X", &src)
        .ok()
        .and_then(|pf| pf.fields.into_iter().next())
}

/// The source line with positions 1-7 (sequence number area, form type,
/// comment position) blank, as the expanded source shows it.
fn blanked(line: &str) -> String {
    let rest: String = line.chars().skip(7).collect();
    format!("{:7}{rest}", "").trim_end().to_string()
}

/// The expanded source's lines (without the sequence numbers' column
/// header): record, key and select/omit lines as written, physical file
/// fields with their type made explicit, usage B, a default column
/// heading, their length in bytes and buffer positions.
fn expanded(input: &Input, errors: &[usize]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 1u32;
    for (i, raw) in input.lines.iter().enumerate() {
        let n = i + 1;
        let line = raw.trim_end();
        if line.trim().is_empty() || col(line, 7, 7) == "*" || errors.contains(&n) {
            continue;
        }
        let name_type = col(line, 17, 17).to_ascii_uppercase();
        let name = col(line, 19, 28).to_ascii_uppercase();
        let s = seq(n);
        if input.physical && name_type.is_empty() && !name.is_empty() {
            let Some(f) = field_of(line) else { continue };
            let len = if f.data_type.fixed_length().is_some() {
                String::new()
            } else {
                f.length.to_string()
            };
            let dec = if f.data_type.is_numeric() {
                f.decimals.to_string()
            } else {
                String::new()
            };
            let mut kw = col(line, 45, 80).to_string();
            if !kw.to_ascii_uppercase().contains("COLHDG(") {
                kw = format!("{kw} COLHDG('{}')", f.name).trim().to_string();
            }
            let spec = format!(
                "{:18}{:<10} {len:>5}{}{dec:>2}B{:6}{kw}",
                "",
                f.name,
                f.data_type.code(),
                ""
            );
            let bytes = f.buffer_bytes();
            out.push(format!("{s:>7}  {spec:<80}{bytes:>6}{pos:>13}{pos:>8}"));
            pos += bytes;
        } else {
            out.push(format!("{s:>7}  {}", blanked(line)).trim_end().to_string());
        }
    }
    out
}

/// The listing's lines. `finals`: the command's messages (ID, severity,
/// text) the summary page ends with; `created`: whether the file was
/// created (the messages page is printed when it was not).
pub fn listing(
    input: &Input,
    msgs: &[Msg],
    finals: &[(String, u32, String)],
    created: bool,
    stamp: &Stamp,
) -> Vec<String> {
    let opt = |o: &str| input.options.iter().any(|x| x.eq_ignore_ascii_case(o));
    let source = !opt("*NOSRC") && !opt("*NOSOURCE");
    let list = !opt("*NOLIST");
    if !source && !list {
        // z_crtpf_nosrc: OPTION(*NOSRC *NOLIST) prints nothing.
        return Vec::new();
    }
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (42, "Data Description"),
            (73, &format!("{}/{}", input.library, input.name)),
            (97, &format!("{} {}", stamp.date, stamp.time)),
            (122, &format!("Page {page:>4}")),
        ])
    };
    let label = |indent: usize, text: &str, value: &str| {
        // The colon in column 54, the value in column 58 (as captured).
        let l = crate::commands::dsp_label(text, 52 - indent);
        format!("{}{l}   {value}", " ".repeat(indent - 1))
            .trim_end()
            .to_string()
    };
    let mut sections: Vec<Vec<String>> = Vec::new();
    let mut first = vec![
        label(2, "File name", &input.name),
        label(4, "Library name", &input.library),
        label(2, "File attribute", &input.attribute),
        label(2, "Source file containing DDS", &input.srcfile.1),
        label(4, "Library name", &input.srcfile.0),
        label(2, "Source member containing DDS", &input.member),
        label(
            2,
            "Source member last changed",
            &input
                .changed
                .as_ref()
                .map(|c| format!("{}  {}", c.date, c.time))
                .unwrap_or_default(),
        ),
        label(
            2,
            "Source listing options",
            &input
                .options
                .iter()
                .map(|o| format!("{o:<11}"))
                .collect::<String>(),
        ),
        label(2, "DDS generation severity level", &input.genlvl),
        label(2, "DDS flagging severity level", &input.flag),
        label(2, "File type", &input.filetype),
        label(2, "Authority", &input.aut),
        label(2, "Replace file", &input.replace),
        label(2, "Text", &input.text),
        label(2, "Compiler", "as4tainr Data Description Processor"),
    ];
    if source {
        first.push(format!("{:38}Data Description Source", ""));
        first.push(
            " SEQNBR  *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8  Date"
                .into(),
        );
        for (i, l) in input.lines.iter().enumerate() {
            first.push(
                format!("{:>7}  {}", seq(i + 1), l.trim_end())
                    .trim_end()
                    .to_string(),
            );
            // The marker: the message ID and -* ending at the position
            // the message is about (z_crtpf_bad: CPD7419 at position 35).
            for m in msgs.iter().filter(|m| m.line == i + 1) {
                let tag = format!("{}-*", m.id.as_deref().unwrap_or(""));
                let width = (8 + m.pos.max(1)).max(tag.len() + 1);
                first.push(format!("*{tag:>width$}"));
            }
        }
        first.push(format!(
            "{:25}* * * * *   E N D   O F   S O U R C E   * * * * *",
            ""
        ));
    }
    sections.push(first);
    if list {
        let errors: Vec<usize> = msgs.iter().map(|m| m.line).collect();
        let mut exp = vec![
            format!("{:42}Expanded Source", ""),
            format!("{:90}Field       Buffer position", ""),
            " SEQNBR  *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8 length        Out     In".into(),
        ];
        exp.extend(expanded(input, &errors));
        exp.push(format!(
            "{:17}* * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *",
            ""
        ));
        sections.push(exp);
    }
    let row = |id: &str, sev: u32, n: &str, text: &str| {
        format!(
            "* {id:<7}{:>8}{n:>9}      Message . . . . :   {text}",
            format!("{sev:02}")
        )
    };
    if !created || !msgs.is_empty() {
        let mut page = vec![
            format!("{:45}Messages", ""),
            "    ID      Severity  Number".to_string(),
        ];
        let mut seen: Vec<(Option<String>, String)> = Vec::new();
        for m in msgs {
            let key = (
                m.id.clone(),
                if m.id.is_some() {
                    String::new()
                } else {
                    m.text.clone()
                },
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let n = msgs
                .iter()
                .filter(|x| x.id == m.id && (m.id.is_some() || x.text == m.text))
                .count();
            page.push(row(
                m.id.as_deref().unwrap_or(""),
                m.sev,
                &n.to_string(),
                &m.text,
            ));
        }
        sections.push(page);
    }
    let count = |lo: u32, hi: u32| msgs.iter().filter(|m| (lo..=hi).contains(&m.sev)).count();
    let mut summary = vec![
        format!("{:42}Message Summary", ""),
        "    Total          Informational          Warning        Error       Severe".into(),
        "                       (0-9)              (10-19)       (20-29)      (30-99)".into(),
        format!(
            "{:>9}{:>18}{:>21}{:>14}{:>13}",
            msgs.len(),
            count(0, 9),
            count(10, 19),
            count(20, 29),
            count(30, 99)
        ),
    ];
    for (id, sev, text) in finals {
        summary.push(row(id, *sev, "", text));
    }
    summary.push(format!(
        "{:25}* * * * *   E N D   O F   C O M P I L A T I O N   * * * * *",
        ""
    ));
    sections.push(summary);
    let mut out = Vec::new();
    let mut page = 0;
    for section in sections {
        for chunk in section.chunks(PAGE - 1) {
            page += 1;
            out.push(header(page));
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(lines: &[&str]) -> Input {
        Input {
            name: "GLD5PF".into(),
            library: "AS4TRAINR1".into(),
            attribute: "Physical".into(),
            srcfile: ("AS4TRAINR1".into(), "GLD5SRC".into()),
            member: "GLD5PF".into(),
            changed: Some(Stamp {
                date: "26-10-27".into(),
                time: "16:45:33".into(),
                zone: String::new(),
            }),
            options: ["*SOURCE", "*LIST", "*NOSECLVL", "*NOEVENTF"]
                .map(String::from)
                .to_vec(),
            genlvl: "20".into(),
            flag: "00".into(),
            filetype: "*DATA".into(),
            aut: "*LIBCRTAUT".into(),
            replace: "*NO".into(),
            text: String::new(),
            lines: lines.iter().map(|l| l.to_string()).collect(),
            physical: true,
        }
    }

    fn stamp() -> Stamp {
        Stamp {
            date: "26-10-27".into(),
            time: "16:45:33".into(),
            zone: String::new(),
        }
    }

    // Expected lines: PUB400 (IBM i 7.5) CRTPF listing, 2026-10-27,
    // tools/golden/verify5.txt z_crtpf (the product line, the compiler's
    // name and the message texts are as4tainr's).
    #[test]
    fn listing_layout() {
        let src = [
            "     A          R GLD5R",
            "     A            CODE           5P 0",
            "     A            NAME          20A",
            "     A          K CODE",
        ];
        let l = listing(
            &input(&src),
            &[],
            &[("CPC7301".into(), 0, "x".into())],
            true,
            &stamp(),
        );
        assert_eq!(
            l[0],
            format!(
                " {}  {}                  Data Description               AS4TRAINR1/GLD5PF       26-10-27 16:45:33        Page    1",
                crate::report::PRODUCT,
                crate::report::LEVEL
            )
        );
        assert_eq!(
            l[1],
            " File name . . . . . . . . . . . . . . . . . . . . . :   GLD5PF"
        );
        assert_eq!(
            l[2],
            "   Library name  . . . . . . . . . . . . . . . . . . :   AS4TRAINR1"
        );
        assert_eq!(
            l[4],
            " Source file containing DDS  . . . . . . . . . . . . :   GLD5SRC"
        );
        assert_eq!(
            l[7],
            " Source member last changed  . . . . . . . . . . . . :   26-10-27  16:45:33"
        );
        assert_eq!(l[8], " Source listing options  . . . . . . . . . . . . . . :   *SOURCE    *LIST      *NOSECLVL  *NOEVENTF");
        assert_eq!(
            l[14],
            " Text  . . . . . . . . . . . . . . . . . . . . . . . :"
        );
        assert_eq!(
            l[16],
            "                                      Data Description Source"
        );
        assert_eq!(l[18], "    100       A          R GLD5R");
        assert_eq!(
            l[22],
            "                         * * * * *   E N D   O F   S O U R C E   * * * * *"
        );
        let e = l
            .iter()
            .position(|x| x.ends_with("Expanded Source"))
            .unwrap();
        assert_eq!(l[e - 1].trim_end(), l[0].replace("Page    1", "Page    2"));
        assert_eq!(l[e + 1], "                                                                                          Field       Buffer position");
        assert_eq!(l[e + 3], "    100                  R GLD5R");
        assert_eq!(l[e + 4], "    200                    CODE           5P 0B      COLHDG('CODE')                           3            1       1");
        assert_eq!(l[e + 5], "    300                    NAME          20A  B      COLHDG('NAME')                          20            4       4");
        assert_eq!(l[e + 6], "    400                  K CODE");
        assert_eq!(
            l[e + 7],
            "                 * * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *"
        );
        // No messages page when the file is created without messages.
        assert!(!l.iter().any(|x| x.trim() == "Messages"));
        let s = l
            .iter()
            .position(|x| x.ends_with("Message Summary"))
            .unwrap();
        assert!(l[s - 1].ends_with("Page    3"));
        assert_eq!(
            l[s + 1],
            "    Total          Informational          Warning        Error       Severe"
        );
        assert_eq!(
            l[s + 3],
            "        0                 0                    0             0            0"
        );
        assert_eq!(
            l[s + 4],
            "* CPC7301      00               Message . . . . :   x"
        );
        assert_eq!(
            l[s + 5],
            "                         * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *"
        );
    }

    // PUB400 2026-10-27, verify5 z_crtpf_bad: the marker under the line,
    // the messages page, the summary's counts; z_crtpf_dup: a messages
    // page with no messages when the file is not created.
    #[test]
    fn errors_and_messages_page() {
        let src = [
            "     A          R GLD5R",
            "     A            CODE           5X 0",
        ];
        let m = Msg {
            id: Some("CPD7419".into()),
            sev: 30,
            line: 2,
            pos: 35,
            text: "t".into(),
        };
        let l = listing(
            &input(&src),
            &[m],
            &[("CPF7302".into(), 40, "y".into())],
            false,
            &stamp(),
        );
        let i = l.iter().position(|x| x.starts_with("    200")).unwrap();
        assert_eq!(l[i + 1], "*                                  CPD7419-*");
        let e = l
            .iter()
            .position(|x| x.ends_with("Expanded Source"))
            .unwrap();
        assert_eq!(l[e + 3], "    100                  R GLD5R");
        assert!(l[e + 4].contains("E N D   O F   E X P A N D E D"));
        let p = l.iter().position(|x| x.trim() == "Messages").unwrap();
        assert!(l[p - 1].ends_with("Page    3"));
        assert_eq!(l[p + 1], "    ID      Severity  Number");
        assert_eq!(
            l[p + 2],
            "* CPD7419      30        1      Message . . . . :   t"
        );
        let s = l
            .iter()
            .position(|x| x.ends_with("Message Summary"))
            .unwrap();
        assert!(l[s - 1].ends_with("Page    4"));
        assert_eq!(
            l[s + 3],
            "        1                 0                    0             0            1"
        );
        assert_eq!(
            l[s + 4],
            "* CPF7302      40               Message . . . . :   y"
        );
        let dup = listing(&input(&src[..1]), &[], &[], false, &stamp());
        let p = dup.iter().position(|x| x.trim() == "Messages").unwrap();
        assert!(dup[p + 2].ends_with("Page    4"));
        let mut nosrc = input(&src);
        nosrc.options = vec!["*NOSRC".into(), "*NOLIST".into()];
        assert!(listing(&nosrc, &[], &[], true, &stamp()).is_empty());
    }
}
