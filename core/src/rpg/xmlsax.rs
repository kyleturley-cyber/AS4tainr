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
//! names (the values IBM i passes); as4tainr's own numbers for the others
//! (docs/fidelity-gaps.md).

use super::*;
use std::cell::RefCell;

/// The least storage an event's string pointer addresses.
const STRING_ROOM: usize = 65_535;

/// The *XML_xxx special words and their event numbers.
const EVENTS: &[(&str, i64)] = &[
    ("*XML_ATTR_NAME", 2),
    ("*XML_ATTR_CHARS", 4),
    ("*XML_CHARS", 5),
    ("*XML_ENCODING_DECL", 10),
    ("*XML_END_DOCUMENT", 12),
    ("*XML_END_ELEMENT", 13),
    ("*XML_START_DOCUMENT", 20),
    ("*XML_START_ELEMENT", 21),
    ("*XML_VERSION_INFO", 25),
    ("*XML_END_ATTR", 26),
    // as4tainr's numbers.
    ("*XML_STANDALONE_DECL", 40),
    ("*XML_DOCTYPE_DECL", 41),
    ("*XML_PREDEF_REF", 42),
    ("*XML_UCS2_REF", 43),
    ("*XML_UNKNOWN_REF", 44),
    ("*XML_ATTR_PREDEF_REF", 45),
    ("*XML_ATTR_UCS2_REF", 46),
    ("*XML_UNKNOWN_ATTR_REF", 47),
    ("*XML_PI_TARGET", 48),
    ("*XML_PI_DATA", 49),
    ("*XML_START_CDATA", 50),
    ("*XML_END_CDATA", 51),
    ("*XML_COMMENT", 52),
    ("*XML_EXCEPTION", 53),
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
    let doc = match roxmltree::Document::parse_with_options(src, opts) {
        Ok(d) => d,
        Err(e) => {
            ev.push((code("*XML_EXCEPTION"), e.to_string(), 1));
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
                if !a.value().is_empty() {
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
            if raw.is_empty() || !raw.contains(['&', '<']) {
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
        for (event, s, exc) in events(&text) {
            let bytes = data::e(&s);
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
                Expr::Bif("_SAXSTRING".into(), vec![]),
                Expr::Num(Dec::from_i64(bytes.len() as i64)),
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
            if exc != 0 {
                bail!("RNX0351: The XML document is not well formed: {s}.");
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
}
