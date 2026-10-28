//! XML-SAX (ILE RPG Reference: XML-SAX, %HANDLER, %XML, "XML events"):
//! an XML document parsed into events, each passed to a handling
//! procedure with the communication area %HANDLER names:
//! handler(commArea : event : pointer to the event's string : its
//! length : exception ID). The string is in the job's CCSID (37). A
//! handler that returns a value other than 0 ends the parsing.
//!
//! Events: *XML_START_DOCUMENT; *XML_VERSION_INFO, *XML_ENCODING_DECL and
//! *XML_STANDALONE_DECL from the XML declaration; *XML_DOCTYPE_DECL;
//! for each element *XML_START_ELEMENT, then each attribute's
//! *XML_ATTR_NAME, *XML_ATTR_CHARS and *XML_END_ATTR, then its content:
//! *XML_CHARS (white space included), *XML_PREDEF_REF for &amp; &lt; &gt;
//! &apos; &quot;, *XML_UCS2_REF for a character reference, *XML_UNKNOWN_REF
//! for another entity, *XML_START_CDATA / *XML_CHARS / *XML_END_CDATA,
//! *XML_COMMENT, *XML_PI_TARGET and *XML_PI_DATA; *XML_END_ELEMENT;
//! *XML_END_DOCUMENT. A document that is not well formed gives
//! *XML_EXCEPTION and the operation ends in error (RNX0351).
//!
//! The events' numbers: those iRPGUnit's SAX_H gives for the events it
//! names (the values IBM i passes), and those IBM i 7.5 passed on PUB400
//! (2026-10-28, tools/golden/verify14.txt u14_rows: *XML_COMMENT 6,
//! *XML_EXCEPTION 15; verify15.txt v15_rows: *XML_ATTR_UCS2_REF 1,
//! *XML_ATTR_PREDEF_REF 3, *XML_UCS2_REF 7, *XML_PREDEF_REF 8,
//! *XML_DOCTYPE_DECL 9, *XML_PI_TARGET 16, *XML_PI_DATA 17,
//! *XML_STANDALONE_DECL 18, *XML_UNKNOWN_REF 24; verify16.txt w16_rows:
//! *XML_END_CDATA 11, *XML_START_CDATA 19, *XML_UNKNOWN_ATTR_REF 23).
//! The parser reads the document in the job's CCSID: on PUB400 (CCSID
//! 273) a CDATA section written with CCSID 37's brackets X'BA' X'BB' was
//! exception 24 and one with 273's X'63' X'FC' parsed (w16_rows D1, D2);
//! as4tainr's jobs are CCSID 37. An event without a string (the start and end
//! of the document, the end of the attributes) has length -1 (u14_rows:
//! "20 -1", "26 -1", "12 -1"). A character reference passes the
//! character in UCS-2 (two bytes, v15_rows "7 2"). An attribute value is
//! given in pieces: its characters, predefined and character references
//! apart (v15_rows: 4 "1", 3 "&", 4 "2", 1). An unknown entity in content
//! is an *XML_UNKNOWN_REF, not an exception (v15_rows D7). An end tag that
//! does not match gives the events before it, then two exceptions, 5 and
//! 300, each with the document up to the end of that tag (v15_rows D8).

use super::*;
use std::cell::RefCell;

/// The least storage an event's string pointer addresses.
const STRING_ROOM: usize = 65_535;

/// The *XML_xxx special words and their event numbers.
const EVENTS: &[(&str, i64)] = &[
    ("*XML_ATTR_UCS2_REF", 1),
    ("*XML_ATTR_NAME", 2),
    ("*XML_ATTR_PREDEF_REF", 3),
    ("*XML_ATTR_CHARS", 4),
    ("*XML_CHARS", 5),
    ("*XML_COMMENT", 6),
    ("*XML_UCS2_REF", 7),
    ("*XML_PREDEF_REF", 8),
    ("*XML_DOCTYPE_DECL", 9),
    ("*XML_ENCODING_DECL", 10),
    ("*XML_END_CDATA", 11),
    ("*XML_END_DOCUMENT", 12),
    ("*XML_END_ELEMENT", 13),
    ("*XML_EXCEPTION", 15),
    ("*XML_PI_TARGET", 16),
    ("*XML_PI_DATA", 17),
    ("*XML_STANDALONE_DECL", 18),
    ("*XML_START_CDATA", 19),
    ("*XML_START_DOCUMENT", 20),
    ("*XML_START_ELEMENT", 21),
    ("*XML_UNKNOWN_ATTR_REF", 23),
    ("*XML_UNKNOWN_REF", 24),
    ("*XML_VERSION_INFO", 25),
    ("*XML_END_ATTR", 26),
];

/// The number of an *XML_xxx special word.
pub fn event_code(word: &str) -> Option<i64> {
    EVENTS
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(word))
        .map(|(_, n)| *n)
}

fn code(word: &str) -> i64 {
    event_code(word).expect("an XML event")
}

/// Events that pass no string: length -1 (u14_rows).
fn no_string(event: i64) -> bool {
    [
        code("*XML_START_DOCUMENT"),
        code("*XML_END_ATTR"),
        code("*XML_END_DOCUMENT"),
    ]
    .contains(&event)
}

thread_local! {
    /// The pointer to the string of the event being handled.
    static CURRENT: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

/// The string pointer of the event being handled (the handler's third
/// argument).
pub(super) fn current_string() -> Value {
    CURRENT.with(|c| c.borrow().last().cloned().unwrap_or(Value::Fig(Fig::Null)))
}

/// One event: its number, its string, its exception ID.
type Event = (i64, String, i64);

/// The events of a document, in order.
pub(super) fn events(text: &str) -> Vec<Event> {
    let mut ev: Vec<Event> = vec![(code("*XML_START_DOCUMENT"), String::new(), 0)];
    let src = text.trim_start_matches('\u{feff}');
    // The XML declaration: version, encoding, standalone.
    let lead = src.trim_start();
    if lead.starts_with("<?xml") {
        if let Some(end) = lead.find("?>") {
            let decl = &lead[5..end];
            for (k, w) in [
                ("version", "*XML_VERSION_INFO"),
                ("encoding", "*XML_ENCODING_DECL"),
                ("standalone", "*XML_STANDALONE_DECL"),
            ] {
                if let Some(v) = pseudo_attr(decl, k) {
                    ev.push((code(w), v, 0));
                }
            }
        }
    }
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    // Unknown entity references are events, not errors (v15_rows D7):
    // marked so that the parser takes them as text.
    let marked = mark_unknown_refs(src);
    let src = marked.as_str();
    let doc = match roxmltree::Document::parse_with_options(src, opts) {
        Ok(d) => d,
        Err(e) => {
            // An end tag that does not match the open element (v15_rows
            // D8, "<a><b></a>"): the events of what came before it, then
            // exceptions 5 and 300 with the document up to the end of the
            // tag. Other errors: one exception (as4tainr's ID 1).
            if let roxmltree::Error::UnexpectedCloseTag(..) = e {
                let at = offset_of(src, e.pos());
                let end = src[at..].find('>').map(|g| at + g + 1).unwrap_or(src.len());
                ev.extend(prefix_events(&src[..at]));
                let upto = src[..end].to_string();
                ev.push((code("*XML_EXCEPTION"), upto.clone(), 5));
                ev.push((code("*XML_EXCEPTION"), upto, 300));
            } else {
                // The events before where it went wrong, then the
                // exception with the document up to there (its length is
                // what the handler gets: the string pointer is *NULL).
                // Markup after "<!" that is no comment, CDATA section or
                // DOCTYPE is exception 24 after the two characters
                // (w16_rows D1: "21 a", then "15 5 24" for "<a><!" and a
                // bracket byte the job's CCSID does not have); other
                // errors as4tainr's ID 1.
                let at = offset_of(src, e.pos()).min(src.len());
                let rest = &src[at..];
                let bad_decl = rest.starts_with("<!")
                    && !["<!--", "<![CDATA[", "<!DOCTYPE"]
                        .iter()
                        .any(|p| rest.starts_with(p));
                ev.extend(prefix_events(&src[..at]));
                if bad_decl {
                    ev.push((code("*XML_EXCEPTION"), src[..at + 2].to_string(), 24));
                } else {
                    ev.push((code("*XML_EXCEPTION"), src[..at].to_string(), 1));
                }
            }
            return ev;
        }
    };
    if let Some(start) = src.find("<!DOCTYPE") {
        if let Some(len) = src[start..].find('>') {
            ev.push((
                code("*XML_DOCTYPE_DECL"),
                src[start..=start + len].to_string(),
                0,
            ));
        }
    }
    for n in doc.root().children() {
        node_events(src, n, &mut ev);
    }
    ev.push((code("*XML_END_DOCUMENT"), String::new(), 0));
    ev
}

/// Private-use characters around an unknown entity's name in the text
/// given to the parser (`&name;` that is not predefined, not a character
/// reference and not declared).
const REF_OPEN: char = '\u{E000}';
const REF_CLOSE: char = '\u{E001}';

fn mark_unknown_refs(src: &str) -> String {
    if src.contains("<!ENTITY") {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find(['&', '<']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // CDATA sections and comments are left as written.
        for (open, close) in [("<![CDATA[", "]]>"), ("<!--", "-->")] {
            if rest.starts_with(open) {
                let end = rest
                    .find(close)
                    .map(|e| e + close.len())
                    .unwrap_or(rest.len());
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            }
        }
        if rest.is_empty() {
            break;
        }
        if let Some(body) = rest.strip_prefix('&') {
            let name_len = body
                .find(|c: char| !(c.is_alphanumeric() || "_:.-".contains(c)))
                .unwrap_or(body.len());
            let name = &body[..name_len];
            if !name.is_empty()
                && body[name_len..].starts_with(';')
                && !matches!(name, "amp" | "lt" | "gt" | "apos" | "quot")
            {
                out.push(REF_OPEN);
                out.push_str(name);
                out.push(REF_CLOSE);
                rest = &body[name_len + 1..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// The byte offset of a parser position (row and column from 1).
fn offset_of(src: &str, pos: roxmltree::TextPos) -> usize {
    let mut off = 0;
    for (i, line) in src.split_inclusive('\n').enumerate() {
        if i + 1 == pos.row as usize {
            return off
                + line
                    .char_indices()
                    .nth(pos.col as usize - 1)
                    .map(|(b, _)| b)
                    .unwrap_or(line.len());
        }
        off += line.len();
    }
    src.len()
}

/// The events of the well-formed start of a document that goes wrong at
/// its end (the elements still open closed to parse it, their ends and
/// the end of the document not given).
fn prefix_events(prefix: &str) -> Vec<Event> {
    let mut open: Vec<String> = Vec::new();
    let mut rest = prefix;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[1..end];
        if let Some(name) = tag.strip_prefix('/') {
            if open.last().map(String::as_str) == Some(name.trim()) {
                open.pop();
            }
        } else if !tag.starts_with(['!', '?']) && !tag.ends_with('/') {
            let name: String = tag.chars().take_while(|c| !c.is_whitespace()).collect();
            open.push(name);
        }
        rest = rest.get(end + 1..).unwrap_or("");
    }
    let closed: String = open.iter().rev().map(|n| format!("</{n}>")).collect();
    let whole = format!("{prefix}{closed}");
    // Only a start that parses (no recursion into another error).
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    if open.is_empty() || roxmltree::Document::parse_with_options(&whole, opts).is_err() {
        return Vec::new();
    }
    let mut ev = events(&whole);
    // Not the closings added, nor the end of the document.
    ev.pop();
    for _ in 0..open.len() {
        ev.pop();
    }
    ev.remove(0); // the start of the document is given already
    ev
}

/// `name="value"` in an XML declaration.
fn pseudo_attr(decl: &str, name: &str) -> Option<String> {
    let at = decl.find(name)?;
    let rest = decl[at + name.len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let body = &rest[1..];
    Some(body[..body.find(q)?].to_string())
}

fn node_events(src: &str, n: roxmltree::Node, ev: &mut Vec<Event>) {
    match n.node_type() {
        roxmltree::NodeType::Element => {
            let name = qualified(n);
            ev.push((code("*XML_START_ELEMENT"), name.clone(), 0));
            for a in n.attributes() {
                let an = match a.namespace().and_then(|ns| n.lookup_prefix(ns)) {
                    Some(p) if !p.is_empty() => format!("{p}:{}", a.name()),
                    _ => a.name().to_string(),
                };
                ev.push((code("*XML_ATTR_NAME"), an, 0));
                let raw = src.get(a.range_value()).unwrap_or("");
                if raw.contains(['&', REF_OPEN]) {
                    attr_events(raw, ev);
                } else if !a.value().is_empty() {
                    ev.push((code("*XML_ATTR_CHARS"), a.value().to_string(), 0));
                }
                ev.push((code("*XML_END_ATTR"), String::new(), 0));
            }
            for c in n.children() {
                node_events(src, c, ev);
            }
            ev.push((code("*XML_END_ELEMENT"), name, 0));
        }
        roxmltree::NodeType::Text => {
            let raw = src.get(n.range()).unwrap_or("");
            // The source text as written gives references and CDATA
            // sections apart; otherwise the text as parsed.
            if raw.is_empty() || !raw.contains(['&', '<', REF_OPEN]) {
                if let Some(t) = n.text().filter(|t| !t.is_empty()) {
                    ev.push((code("*XML_CHARS"), t.to_string(), 0));
                }
            } else {
                text_events(raw, ev);
            }
        }
        roxmltree::NodeType::Comment => {
            ev.push((code("*XML_COMMENT"), n.text().unwrap_or("").to_string(), 0));
        }
        roxmltree::NodeType::PI => {
            if let Some(pi) = n.pi() {
                ev.push((code("*XML_PI_TARGET"), pi.target.to_string(), 0));
                if let Some(v) = pi.value {
                    ev.push((code("*XML_PI_DATA"), v.to_string(), 0));
                }
            }
        }
        roxmltree::NodeType::Root => {}
    }
}

fn qualified(n: roxmltree::Node) -> String {
    let local = n.tag_name().name();
    match n.tag_name().namespace().and_then(|ns| n.lookup_prefix(ns)) {
        Some(p) if !p.is_empty() => format!("{p}:{local}"),
        _ => local.to_string(),
    }
}

/// Character data as written: runs of characters, references and CDATA
/// sections.
fn text_events(raw: &str, ev: &mut Vec<Event>) {
    let mut chars = String::new();
    let flush = |chars: &mut String, ev: &mut Vec<Event>| {
        if !chars.is_empty() {
            ev.push((code("*XML_CHARS"), std::mem::take(chars), 0));
        }
    };
    let mut rest = raw;
    while !rest.is_empty() {
        if let Some(body) = rest.strip_prefix("<![CDATA[") {
            flush(&mut chars, ev);
            let end = body.find("]]>").unwrap_or(body.len());
            ev.push((code("*XML_START_CDATA"), "<![CDATA[".into(), 0));
            if end > 0 {
                ev.push((code("*XML_CHARS"), body[..end].to_string(), 0));
            }
            ev.push((code("*XML_END_CDATA"), "]]>".into(), 0));
            rest = body.get(end + 3..).unwrap_or("");
            continue;
        }
        if let Some(body) = rest.strip_prefix(REF_OPEN) {
            if let Some(close) = body.find(REF_CLOSE) {
                flush(&mut chars, ev);
                ev.push((code("*XML_UNKNOWN_REF"), body[..close].to_string(), 0));
                rest = &body[close + REF_CLOSE.len_utf8()..];
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix('&') {
            if let Some(semi) = body.find(';') {
                flush(&mut chars, ev);
                let name = &body[..semi];
                let (w, v) = match name {
                    "amp" => ("*XML_PREDEF_REF", "&".to_string()),
                    "lt" => ("*XML_PREDEF_REF", "<".to_string()),
                    "gt" => ("*XML_PREDEF_REF", ">".to_string()),
                    "apos" => ("*XML_PREDEF_REF", "'".to_string()),
                    "quot" => ("*XML_PREDEF_REF", "\"".to_string()),
                    n if n.starts_with('#') => {
                        let num = if let Some(h) = n.strip_prefix("#x") {
                            u32::from_str_radix(h, 16).ok()
                        } else {
                            n[1..].parse().ok()
                        };
                        (
                            "*XML_UCS2_REF",
                            num.and_then(char::from_u32)
                                .map(String::from)
                                .unwrap_or_default(),
                        )
                    }
                    n => ("*XML_UNKNOWN_REF", n.to_string()),
                };
                ev.push((code(w), v, 0));
                rest = &body[semi + 1..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        chars.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut chars, ev);
}

/// An attribute value as written: its characters, predefined and
/// character references apart (v15_rows D6: `x="1&amp;2&#65;"` gives
/// 4 "1", 3 "&", 4 "2", 1 "A").
fn attr_events(raw: &str, ev: &mut Vec<Event>) {
    let mut sub = Vec::new();
    text_events(raw, &mut sub);
    for (c, v, x) in sub {
        let w = match EVENTS.iter().find(|(_, n)| *n == c).map(|e| e.0) {
            Some("*XML_PREDEF_REF") => "*XML_ATTR_PREDEF_REF",
            Some("*XML_UCS2_REF") => "*XML_ATTR_UCS2_REF",
            Some("*XML_UNKNOWN_REF") => "*XML_UNKNOWN_ATTR_REF",
            _ => "*XML_ATTR_CHARS",
        };
        ev.push((code(w), v, x));
    }
}

/// An event's string as the handler gets it: a character reference's
/// character in UCS-2 (v15_rows "7 2", "1 2"), the others in the job's
/// CCSID.
fn event_bytes(event: i64, s: &str) -> Vec<u8> {
    if event == code("*XML_UCS2_REF") || event == code("*XML_ATTR_UCS2_REF") {
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    } else {
        data::e(s)
    }
}

impl Machine<'_> {
    /// XML-SAX %HANDLER(procedure : communication area) %XML(document :
    /// options): options doc=string (default) or doc=file (an IFS path),
    /// ccsid (accepted).
    pub(super) fn xml_sax(&mut self, handler: &Expr, xml: &[Expr]) -> Result<()> {
        let Expr::Bif(b, hargs) = handler else {
            bail!("XML-SAX needs %HANDLER");
        };
        if b != "HANDLER" || hargs.len() != 2 {
            bail!("XML-SAX needs %HANDLER(procedure : communication area)");
        }
        let proc = match &hargs[0] {
            Expr::Name { parts, args: None } => parts.join("."),
            other => bail!("%HANDLER: a procedure is needed, found {other:?}"),
        };
        let doc = self.eval(
            xml.first()
                .ok_or_else(|| anyhow!("%XML needs a document"))?,
        )?;
        let opts = match xml.get(1) {
            Some(e) => a(&self.eval(e)?.chars()?),
            None => String::new(),
        };
        let mut file = false;
        for w in opts.split_whitespace() {
            let (k, v) = w
                .split_once('=')
                .ok_or_else(|| anyhow!("RNX0352: The XML option {w} is not valid."))?;
            match (
                k.to_ascii_lowercase().as_str(),
                v.to_ascii_lowercase().as_str(),
            ) {
                ("doc", "file") => file = true,
                ("doc", "string") => file = false,
                ("ccsid", _) => {}
                _ => bail!("RNX0352: The XML option {w} is not valid."),
            }
        }
        let text = if file {
            let path = a(&doc.chars()?).trim().to_string();
            // What this job wrote to files it still has open is in them
            // (IBM i: a write is seen by other opens at once).
            self.c_flush();
            let (bytes, ccsid) = self
                .block(crate::ifs::read_file(self.io.client, &path))
                .map_err(|e| anyhow!("RNX0351: The XML document {path} cannot be read: {e:#}."))?;
            if ccsid == 37 {
                crate::tn5250::ebcdic::decode(&bytes)
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            }
        } else {
            a(&doc.chars()?)
        };
        let comm = hargs[1].clone();
        let all = events(&text);
        for (i, (event, s, exc)) in all.iter().cloned().enumerate() {
            let bytes = event_bytes(event, &s);
            // The parser's buffer: longer than the string, as a handler's
            // based variable over it usually is (CHAR(2048) and the like).
            let room = bytes.len().max(STRING_ROOM);
            let block = self.alloc(&Value::Num(Dec::from_i64(room as i64)))?;
            let (buf, off) = self.addr(&block, bytes.len())?;
            self.bufs[buf][off..off + bytes.len()].copy_from_slice(&bytes);
            CURRENT.with(|c| c.borrow_mut().push(block.clone()));
            let args = [
                comm.clone(),
                Expr::Num(Dec::from_i64(event)),
                // An exception's string pointer is *NULL, its length that
                // of the document read (v15_rows D8 "15 10 5" with no
                // text; u14_rows "15 47 24").
                if event == code("*XML_EXCEPTION") {
                    Expr::Fig(Fig::Null)
                } else {
                    Expr::Bif("_SAXSTRING".into(), vec![])
                },
                Expr::Num(Dec::from_i64(if no_string(event) {
                    -1
                } else {
                    bytes.len() as i64
                })),
                Expr::Num(Dec::from_i64(exc)),
            ];
            let r = self.call_named(&proc, &args);
            CURRENT.with(|c| c.borrow_mut().pop());
            // The buffer goes for good (a freed block otherwise stays
            // readable).
            if let Ok((b, _)) = self.addr(&block, 0) {
                let _ = self.dealloc(&block);
                if let Some(x) = self.bufs.get_mut(b) {
                    *x = Vec::new();
                }
            }
            let rc = match r? {
                Some(v) => v.num().ok().and_then(|d| d.to_i128().ok()).unwrap_or(0),
                None => 0,
            };
            // The operation ends in error after the last of the
            // exceptions (v15_rows D8: two, then RNX0351).
            let more_exceptions = all
                .get(i + 1)
                .is_some_and(|n| n.0 == code("*XML_EXCEPTION"));
            if exc != 0 && !(more_exceptions && rc == 0) {
                bail!("RNX0351: The XML document is not well formed.");
            }
            if rc != 0 {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ev: &[Event]) -> Vec<String> {
        ev.iter()
            .map(|(c, s, _)| {
                let w = EVENTS.iter().find(|(_, n)| n == c).unwrap().0;
                if s.is_empty() {
                    w.to_string()
                } else {
                    format!("{w} {s}")
                }
            })
            .collect()
    }

    #[test]
    fn events_in_document_order() {
        // ILE RPG Reference, "XML events": the order of the events of a
        // document with a declaration, attributes, a predefined entity
        // reference, a CDATA section and a comment.
        let ev = events(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><a x=\"1\"><b>p &amp; q</b><![CDATA[<c>]]><!--n--></a>",
        );
        assert_eq!(
            names(&ev),
            [
                "*XML_START_DOCUMENT",
                "*XML_VERSION_INFO 1.0",
                "*XML_ENCODING_DECL UTF-8",
                "*XML_START_ELEMENT a",
                "*XML_ATTR_NAME x",
                "*XML_ATTR_CHARS 1",
                "*XML_END_ATTR",
                "*XML_START_ELEMENT b",
                "*XML_CHARS p ",
                "*XML_PREDEF_REF &",
                "*XML_CHARS  q",
                "*XML_END_ELEMENT b",
                "*XML_START_CDATA <![CDATA[",
                "*XML_CHARS <c>",
                "*XML_END_CDATA ]]>",
                "*XML_COMMENT n",
                "*XML_END_ELEMENT a",
                "*XML_END_DOCUMENT",
            ]
        );
        let bad = events("<a><b></a>");
        assert_eq!(bad.last().unwrap().0, code("*XML_EXCEPTION"));
    }

    // PUB400 2026-10-28, tools/golden/verify14.txt u14_rows: IBM i 7.5's
    // numbers for a comment (6, its text with the blanks around it) and
    // an exception (15); no string for the document's start and end and
    // the end of the attributes.
    #[test]
    fn captured_numbers() {
        assert_eq!(code("*XML_COMMENT"), 6);
        assert_eq!(code("*XML_EXCEPTION"), 15);
        let ev = events("<a x=\"1\">t<!-- c --></a>");
        assert!(ev.contains(&(6, " c ".to_string(), 0)), "{ev:?}");
        assert!(no_string(20) && no_string(26) && no_string(12) && !no_string(5));
    }

    // PUB400 2026-10-28, tools/golden/verify15.txt v15_rows: the numbers
    // and strings IBM i 7.5 passed for one kind of event a document.
    #[test]
    fn verify15_events() {
        let nums = |d: &str| -> Vec<(i64, String, i64)> { events(d) };
        // D1: predefined references 8, character references 7 in UCS-2.
        let d1 = nums("<a>p &amp; q &#65;</a>");
        assert!(
            d1.contains(&(8, "&".into(), 0)) && d1.contains(&(7, "A".into(), 0)),
            "{d1:?}"
        );
        assert_eq!(event_bytes(7, "A"), [0x00, 0x41]);
        // D3: PI target 16 and data 17; D4: standalone 18; D5: DOCTYPE 9.
        let d3 = nums("<?pi data?><a/>");
        assert!(d3.contains(&(16, "pi".into(), 0)) && d3.contains(&(17, "data".into(), 0)));
        let d4 = nums("<?xml version=\"1.0\" encoding=\"IBM-037\" standalone=\"yes\"?><a/>");
        assert!(d4.contains(&(18, "yes".into(), 0)));
        let d5 = nums("<!DOCTYPE a><a/>");
        assert!(d5.contains(&(9, "<!DOCTYPE a>".into(), 0)));
        // D6: an attribute value in pieces: 4 "1", 3 "&", 4 "2", 1 "A".
        let d6 = nums("<a x=\"1&amp;2&#65;\"/>");
        let attr: Vec<(i64, String)> = d6
            .iter()
            .filter(|e| [1, 3, 4].contains(&e.0))
            .map(|e| (e.0, e.1.clone()))
            .collect();
        assert_eq!(
            attr,
            [
                (4, "1".into()),
                (3, "&".into()),
                (4, "2".into()),
                (1, "A".into())
            ]
        );
        // D7: an unknown entity is event 24 with its name.
        let d7 = nums("<a>&x;</a>");
        assert_eq!(
            d7.iter().map(|e| e.0).collect::<Vec<_>>(),
            [20, 21, 24, 13, 12]
        );
        assert_eq!(d7[2].1, "x");
        // verify16 w16_rows D2, D4: CDATA 19 and 11 around its characters,
        // an unknown entity in an attribute 23.
        let c = nums("<a><![CDATA[d]]></a>");
        assert!(c.contains(&(19, "<![CDATA[".into(), 0)) && c.contains(&(11, "]]>".into(), 0)));
        let u = nums("<a x=\"1&y;2\"/>");
        assert!(u.contains(&(23, "y".into(), 0)), "{u:?}");
        // w16_rows D1 (seen from the other side: a bracket byte the job's
        // CCSID does not have): "<a>", then exception 24 with 5 bytes.
        let b = nums("<a><!\u{c4}CDATA\u{c4}d]]></a>");
        let codes: Vec<(i64, i64, usize)> = b.iter().map(|e| (e.0, e.2, e.1.len())).collect();
        assert_eq!(codes, [(20, 0, 0), (21, 0, 1), (15, 24, 5)]);
        // Documents that go wrong anywhere end in one exception or two.
        for bad in [
            "",
            "<",
            "<a",
            "&",
            "x",
            "<a><!",
            "<a></b>",
            "<a><b><c></a>",
            "<a>&#;</a>",
        ] {
            let ev = nums(bad);
            assert_eq!(ev.last().map(|e| e.0), Some(15), "{bad}: {ev:?}");
        }
        // D8: an end tag that does not match: the starts before it, then
        // exceptions 5 and 300, each with the 10 bytes read.
        let d8 = nums("<a><b></a>");
        let codes: Vec<(i64, i64, usize)> = d8.iter().map(|e| (e.0, e.2, e.1.len())).collect();
        assert_eq!(
            codes,
            [
                (20, 0, 0),
                (21, 0, 1),
                (21, 0, 1),
                (15, 5, 10),
                (15, 300, 10)
            ]
        );
    }
}
