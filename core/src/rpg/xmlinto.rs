//! XML-INTO (ILE RPG Reference: XML-INTO, %XML, "%XML options for the
//! XML-INTO operation code", "Path option"): an XML document parsed into
//! a variable. The receiver's name (or the path option) says which
//! element holds its value: a scalar or data structure is the outermost
//! element (or the element the path names); an array is the series of
//! elements of its name under the outermost element (or the path's last
//! item). A data structure's subfields are its child elements or
//! attributes of the same name. Names are matched by the case option
//! (lower by default: the XML names are in lower case); character data is
//! trimmed of blanks, tabs and line ends and its runs of them become one
//! blank (trim=all, the default); a subfield the document lacks, or an
//! element or attribute no subfield takes, ends the operation in status
//! 00353 unless allowmissing=yes or allowextra=yes.
//!
//! %HANDLER is not done; datasubf and countprefix are.

use super::*;

/// The %XML options (ILE RPG Reference, "%XML options for the XML-INTO
/// operation code").
#[derive(Debug, Clone)]
pub(super) struct XmlOpts {
    pub file: bool,
    pub path: Vec<String>,
    pub case: Case,
    pub trim: bool,
    pub allow_missing: bool,
    pub allow_extra: bool,
    pub datasubf: Option<String>,
    pub countprefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Case {
    Lower,
    Upper,
    Any,
    Convert,
}

impl XmlOpts {
    /// `opt=value opt=value`: names and values in any case, the last of
    /// an option counts; a bad option or value is status 00352.
    pub(super) fn parse(text: &str) -> Result<XmlOpts> {
        let mut o = XmlOpts {
            file: false,
            path: Vec::new(),
            case: Case::Lower,
            trim: true,
            allow_missing: false,
            allow_extra: false,
            datasubf: None,
            countprefix: None,
        };
        let bad = |w: &str| anyhow!("RNX0352: The XML option {w} is not valid.");
        for w in text.split_whitespace() {
            let (k, v) = w.split_once('=').ok_or_else(|| bad(w))?;
            let lv = v.to_ascii_lowercase();
            let yes_no = || match lv.as_str() {
                "yes" => Ok(true),
                "no" => Ok(false),
                _ => Err(bad(w)),
            };
            match k.to_ascii_lowercase().as_str() {
                "doc" => {
                    o.file = match lv.as_str() {
                        "string" => false,
                        "file" => true,
                        _ => return Err(bad(w)),
                    }
                }
                "path" => {
                    o.path = v
                        .split('/')
                        .filter(|p| !p.is_empty())
                        .map(String::from)
                        .collect()
                }
                "case" => {
                    o.case = match lv.as_str() {
                        "lower" => Case::Lower,
                        "upper" => Case::Upper,
                        "any" => Case::Any,
                        "convert" => Case::Convert,
                        _ => return Err(bad(w)),
                    }
                }
                "trim" => {
                    o.trim = match lv.as_str() {
                        "all" => true,
                        "none" => false,
                        _ => return Err(bad(w)),
                    }
                }
                "allowmissing" => o.allow_missing = yes_no()?,
                "allowextra" => o.allow_extra = yes_no()?,
                "datasubf" => o.datasubf = Some(v.to_ascii_uppercase()),
                "countprefix" => o.countprefix = Some(v.to_ascii_uppercase()),
                // The document's CCSID and namespace handling: the
                // document is Unicode text here; names keep their prefix.
                "ccsid" | "ns" | "nsprefix" => {}
                _ => return Err(bad(w)),
            }
        }
        Ok(o)
    }

    /// Does an XML name match an RPG name (or a path item, which is
    /// written as in the document unless the case option says how)?
    fn matches(&self, xml: &str, rpg: &str, path_item: bool) -> bool {
        if path_item && self.case == Case::Lower {
            return xml == rpg;
        }
        match self.case {
            Case::Lower => xml == rpg.to_ascii_lowercase(),
            Case::Upper => xml == rpg.to_ascii_uppercase(),
            Case::Any => xml.eq_ignore_ascii_case(rpg),
            Case::Convert => {
                let conv: String = xml
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                conv.eq_ignore_ascii_case(rpg)
            }
        }
    }

    /// Character data as it is assigned.
    fn text(&self, s: &str) -> String {
        if !self.trim {
            return s.to_string();
        }
        s.split([' ', '\t', '\n', '\r'])
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn no_match(why: String) -> anyhow::Error {
    anyhow!("RNX0353: The XML document does not match the RPG variable: {why}.")
}

/// A place to assign: a name's parts, and the index of each part that is
/// an array element.
#[derive(Clone, Debug)]
struct Lv {
    parts: Vec<String>,
    idx: Vec<Option<i128>>,
}

impl Lv {
    fn key(&self) -> String {
        self.parts.join(".")
    }

    fn name(&self) -> &str {
        self.parts.last().map(|s| s.as_str()).unwrap_or("")
    }

    fn expr(&self) -> Expr {
        let num = |k: i128| Expr::Num(Dec::new(k, 0));
        let n = self.parts.len();
        let args = self.idx[n - 1].map(|k| vec![num(k)]);
        if self.idx[..n - 1].iter().all(|i| i.is_none()) {
            return Expr::Name {
                parts: self.parts.clone(),
                args,
            };
        }
        Expr::Path {
            parts: self.parts.clone(),
            idx: self.idx[..n - 1]
                .iter()
                .map(|i| i.map(|k| Box::new(num(k))))
                .collect(),
            args,
        }
    }

    fn with_index(&self, k: i128) -> Lv {
        let mut l = self.clone();
        let n = l.idx.len();
        l.idx[n - 1] = Some(k);
        l
    }
}

impl Machine<'_> {
    /// XML-INTO receiver %XML(document : options).
    pub(super) fn xml_into(&mut self, target: &Expr, xml: &[Expr]) -> Result<()> {
        let doc = self.eval(
            xml.first()
                .ok_or_else(|| anyhow!("%XML needs a document"))?,
        )?;
        let opts = match xml.get(1) {
            Some(e) => a(&self.eval(e)?.chars()?),
            None => String::new(),
        };
        let o = XmlOpts::parse(&opts)?;
        let text = if o.file {
            let path = a(&doc.chars()?).trim().to_string();
            // What this job wrote to files it still has open is in them.
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
        let popt = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let parsed = roxmltree::Document::parse_with_options(text.trim(), popt)
            .map_err(|e| anyhow!("RNX0351: The XML document is not valid: {e}."))?;
        let root = parsed.root_element();
        let (parts, idx) = match target {
            Expr::Name { parts, args: None } => (parts.clone(), vec![None; parts.len()]),
            other => bail!("XML-INTO: the receiver must be a variable, found {other:?}"),
        };
        let lv = Lv { parts, idx };
        let rname = lv.name().to_string();
        let array = self.dim_of(&lv.key()) > 0;
        if array {
            // The elements of the array's name under the outermost
            // element, or the path's last item under the items before.
            let (parents, item): (Vec<roxmltree::Node>, String) = if o.path.is_empty() {
                (vec![root], rname.clone())
            } else {
                let (last, before) = o.path.split_last().unwrap();
                (self.xml_path(&o, root, before)?, last.clone())
            };
            let path_named = !o.path.is_empty();
            let elems: Vec<roxmltree::Node> = parents
                .iter()
                .flat_map(|p| p.children().filter(|c| c.is_element()))
                .filter(|c| o.matches(c.tag_name().name(), &item, path_named))
                .collect();
            let dim = self.dim_of(&lv.key()) as usize;
            for (k, el) in elems.iter().take(dim).enumerate() {
                self.xml_fill(&o, *el, &lv.with_index(k as i128 + 1))?;
            }
            return Ok(());
        }
        let node = if o.path.is_empty() {
            if !o.matches(root.tag_name().name(), &rname, false) {
                return Err(no_match(format!(
                    "the outermost element is {}, not {}",
                    root.tag_name().name(),
                    rname.to_ascii_lowercase()
                )));
            }
            root
        } else {
            let (last, before) = o.path.split_last().unwrap();
            let parents = self.xml_path(&o, root, before)?;
            parents
                .iter()
                .flat_map(|p| {
                    let v: Vec<roxmltree::Node> = if before.is_empty() {
                        vec![*p]
                    } else {
                        p.children().filter(|c| c.is_element()).collect()
                    };
                    v
                })
                .find(|c| o.matches(c.tag_name().name(), last, true))
                .ok_or_else(|| no_match(format!("no element for path {}", o.path.join("/"))))?
        };
        self.xml_fill(&o, node, &lv)
    }

    /// The elements a path's items lead to (the first item is the
    /// outermost element).
    fn xml_path<'a, 'i>(
        &self,
        o: &XmlOpts,
        root: roxmltree::Node<'a, 'i>,
        items: &[String],
    ) -> Result<Vec<roxmltree::Node<'a, 'i>>> {
        let Some((first, rest)) = items.split_first() else {
            return Ok(vec![root]);
        };
        if !o.matches(root.tag_name().name(), first, true) {
            return Err(no_match(format!("no element for path item {first}")));
        }
        let mut at = vec![root];
        for it in rest {
            at = at
                .iter()
                .flat_map(|n| n.children().filter(|c| c.is_element()))
                .filter(|c| o.matches(c.tag_name().name(), it, true))
                .collect();
            if at.is_empty() {
                return Err(no_match(format!("no element for path item {it}")));
            }
        }
        Ok(at)
    }

    fn dim_of(&self, key: &str) -> u32 {
        match self.sym(key) {
            Some(Sym::Var(v)) => v.dim,
            Some(Sym::Ds(d)) => d.dim,
            _ => 0,
        }
    }

    /// An element's content into a variable (or array element).
    fn xml_fill(&mut self, o: &XmlOpts, node: roxmltree::Node, lv: &Lv) -> Result<()> {
        match self.sym(&lv.key()) {
            Some(Sym::Ds(d)) => {
                let d = d.clone();
                self.xml_ds(o, node, lv, &d)
            }
            Some(Sym::Var(_)) => {
                let text: String = node
                    .children()
                    .filter(|c| c.is_text())
                    .filter_map(|c| c.text())
                    .collect();
                self.xml_value(o, &text, lv)
            }
            _ => bail!("XML-INTO: {} is not a variable", lv.key()),
        }
    }

    /// A data structure's subfields from an element's children and
    /// attributes.
    fn xml_ds(
        &mut self,
        o: &XmlOpts,
        node: roxmltree::Node,
        lv: &Lv,
        d: &crate::rpg::program::DsInfo,
    ) -> Result<()> {
        let sub = |sf: &str| -> Lv {
            if d.qualified {
                let mut l = lv.clone();
                l.parts.push(sf.to_string());
                l.idx.push(None);
                l
            } else {
                Lv {
                    parts: vec![sf.to_string()],
                    idx: vec![None],
                }
            }
        };
        let children: Vec<roxmltree::Node> = node.children().filter(|c| c.is_element()).collect();
        let mut used = vec![false; children.len()];
        let mut used_attr = vec![false; node.attributes().len()];
        let counted: Vec<String> = match &o.countprefix {
            Some(p) => d
                .subfields
                .iter()
                .filter_map(|s| s.strip_prefix(p.as_str()).map(String::from))
                .collect(),
            None => Vec::new(),
        };
        for sf in &d.subfields {
            let sl = sub(sf);
            // The data subfield: the element's own text.
            if o.datasubf.as_deref() == Some(sf.as_str()) {
                let text: String = node
                    .children()
                    .filter(|c| c.is_text())
                    .filter_map(|c| c.text())
                    .collect();
                self.xml_value(o, &text, &sl)?;
                continue;
            }
            // A count subfield: set with its array's count below.
            if let Some(p) = &o.countprefix {
                if let Some(of) = sf.strip_prefix(p.as_str()) {
                    if d.subfields.iter().any(|s| s == of) {
                        continue;
                    }
                }
            }
            let dim = self.dim_of(&sl.key());
            let hits: Vec<usize> = children
                .iter()
                .enumerate()
                .filter(|(_, c)| o.matches(c.tag_name().name(), sf, false))
                .map(|(k, _)| k)
                .collect();
            if dim > 0 {
                let n = hits.len().min(dim as usize);
                for (k, h) in hits.iter().take(dim as usize).enumerate() {
                    used[*h] = true;
                    self.xml_fill(o, children[*h], &sl.with_index(k as i128 + 1))?;
                }
                if hits.len() > dim as usize && !o.allow_extra {
                    return Err(no_match(format!(
                        "more {} elements than {} has elements",
                        sf.to_ascii_lowercase(),
                        sl.key()
                    )));
                }
                if counted.iter().any(|c| c == sf) {
                    let p = o.countprefix.clone().unwrap_or_default();
                    let cl = sub(&format!("{p}{sf}"));
                    self.assign(
                        &cl.expr(),
                        &Value::Num(Dec::new(n as i128, 0)),
                        Fit::Truncate { half_adjust: false },
                        false,
                    )?;
                } else if (n as u32) < dim && !o.allow_missing {
                    return Err(no_match(format!(
                        "{} {} elements for the {dim} of {}",
                        n,
                        sf.to_ascii_lowercase(),
                        sl.key()
                    )));
                }
                continue;
            }
            match hits.as_slice() {
                [h] => {
                    used[*h] = true;
                    self.xml_fill(o, children[*h], &sl)?;
                }
                [] => {
                    // An attribute of the name.
                    let attr = node
                        .attributes()
                        .enumerate()
                        .find(|(_, at)| o.matches(at.name(), sf, false));
                    match attr {
                        Some((k, at)) => {
                            used_attr[k] = true;
                            let v = at.value().to_string();
                            self.xml_value(o, &v, &sl)?;
                        }
                        None if o.allow_missing => {}
                        None => {
                            return Err(no_match(format!(
                                "no element or attribute {} in {}",
                                sf.to_ascii_lowercase(),
                                node.tag_name().name()
                            )))
                        }
                    }
                }
                [h, ..] => {
                    if !o.allow_extra {
                        return Err(no_match(format!(
                            "more than one {} element in {}",
                            sf.to_ascii_lowercase(),
                            node.tag_name().name()
                        )));
                    }
                    used[*h] = true;
                    self.xml_fill(o, children[*h], &sl)?;
                }
            }
        }
        if !o.allow_extra {
            if let Some((k, _)) = used.iter().enumerate().find(|(_, u)| !**u) {
                return Err(no_match(format!(
                    "element {} in {} is not a subfield of {}",
                    children[k].tag_name().name(),
                    node.tag_name().name(),
                    lv.key()
                )));
            }
            if let Some((k, _)) = used_attr.iter().enumerate().find(|(_, u)| !**u) {
                let at = node.attributes().nth(k).unwrap();
                // Namespace declarations are not data.
                if !at.name().starts_with("xmlns") {
                    return Err(no_match(format!(
                        "attribute {} of {} is not a subfield of {}",
                        at.name(),
                        node.tag_name().name(),
                        lv.key()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Character data into a scalar, converted as its type needs.
    fn xml_value(&mut self, o: &XmlOpts, raw: &str, lv: &Lv) -> Result<()> {
        let t = match self.sym(&lv.key()) {
            Some(Sym::Var(v)) => v.dtype,
            _ => bail!("XML-INTO: {} is not a scalar", lv.key()),
        };
        let bad = |why: &str| {
            no_match(format!(
                "'{}' is not valid for {} ({why})",
                raw.trim(),
                lv.key()
            ))
        };
        let v = match t {
            t if t.is_numeric() => {
                let s = raw.trim();
                if matches!(t, DType::Float(_)) {
                    let x: f64 = s.parse().map_err(|_| bad("a number is expected"))?;
                    Value::Num(data::float_dec(x)?)
                } else {
                    Value::Num(Dec::parse(s).map_err(|_| bad("a number is expected"))?)
                }
            }
            DType::Ind => match raw.trim() {
                "1" => Value::Ind(true),
                "0" | "" => Value::Ind(false),
                _ => return Err(bad("an indicator is 0 or 1")),
            },
            DType::Date(f) => {
                use crate::rpg::dates;
                let s = raw.trim();
                let d = dates::parse_date(s, &f.or(self.prog.datfmt()?))
                    .or_else(|_| dates::parse_date(s, &dates::DateFmt::iso()))
                    .map_err(|_| bad("a date is expected"))?;
                Value::str(&dates::format_date(d, &dates::DateFmt::iso()))
            }
            DType::Time(f) => {
                use crate::rpg::dates;
                let s = raw.trim();
                let h = dates::parse_time(s, &f.or(self.prog.timfmt()?))
                    .or_else(|_| dates::parse_time(s, &dates::TimeFmt::iso()))
                    .map_err(|_| bad("a time is expected"))?;
                Value::str(&dates::format_time(h, &dates::TimeFmt::iso()))
            }
            DType::Timestamp(_) => {
                use crate::rpg::dates;
                let ts = dates::parse_ts(raw.trim()).map_err(|_| bad("a timestamp is expected"))?;
                Value::str(&dates::format_ts(&ts, false))
            }
            _ => Value::str(&o.text(raw)),
        };
        let fit = Fit::Truncate { half_adjust: false };
        self.assign(&lv.expr(), &v, fit, false)
    }
}

impl Machine<'_> {
    /// DATA-INTO: the parser program reports the document through the
    /// QrnDi* procedures (ILE RPG Reference, "DATA-INTO"), which as4tainr
    /// does not have yet.
    pub(super) fn data_into(
        &mut self,
        _target: &Expr,
        _data: &[Expr],
        parser: &Option<Vec<Expr>>,
    ) -> Result<()> {
        let name = match parser.as_ref().and_then(|p| p.first()) {
            Some(e) => a(&self.eval(e)?.chars()?).trim().to_string(),
            None => String::new(),
        };
        bail!("DATA-INTO is not supported yet (parser {name}: the QrnDi* parser interface is not there)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        // %XML options: name=value pairs, the last of a name counts, an
        // unknown value is status 00352 (ILE RPG Reference, %XML).
        let o = XmlOpts::parse("case=any  path=a/b trim=none case=upper").unwrap();
        assert_eq!(o.case, Case::Upper);
        assert_eq!(o.path, ["a", "b"]);
        assert!(!o.trim);
        assert!(XmlOpts::parse("allowextra=ok")
            .unwrap_err()
            .to_string()
            .starts_with("RNX0352"));
        let d = XmlOpts::parse("").unwrap();
        assert!(d.matches("name", "NAME", false) && !d.matches("Name", "NAME", false));
        assert_eq!(d.text("  a \n\t b  "), "a b");
    }
}
