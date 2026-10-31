//! The fields of a logical file over its physical file, as SQL.
//!
//! Written from IBM's DDS reference for physical and logical files
//! (positions 30-38, "Conversion of one numeric data type to another",
//! "Conversion between zoned decimal and character", decimal positions,
//! RENAME, CONCAT, SST). Each field of the logical file's record format is
//! an expression over the physical file's columns: the view a program
//! reads and the access path's index are made of them. A field a program
//! may change (usage B) also says which physical file columns it sets when
//! a record is written or updated through the logical file; when the view
//! cannot be changed as it is (a field converted, concatenated or input
//! only, one physical field in two fields), `catalog::create_logical_file`
//! gives it a trigger that does it.

use crate::catalog::ident;
use crate::dds::{DataType, Decimals, FieldDef, LfField, LfSource, LogicalFile};
use anyhow::{anyhow, bail, Result};

/// A field of the logical file's record format.
#[derive(Debug, Clone)]
pub struct Column {
    /// The field as programs see it.
    pub def: FieldDef,
    /// B (both) or I (input only).
    pub usage: char,
    /// Its value over the physical file's columns.
    pub read: String,
    /// `read` is a physical file column as it is.
    pub plain: bool,
    /// For usage B: the physical file columns it sets, with their values
    /// from `NEW.<view column>`.
    pub writes: Vec<(String, String)>,
    /// The physical file fields named on its RENAME or CONCAT keyword.
    pub params: Vec<String>,
    /// The view column's default (the physical field's), as SQL.
    pub default: Option<String>,
}

/// The physical file field of a name.
fn pf_field<'a>(pf: &'a [FieldDef], name: &str, file: &str) -> Result<&'a FieldDef> {
    pf.iter()
        .find(|f| f.name == name)
        .ok_or_else(|| anyhow!("field {name} is not in physical file {file}"))
}

/// The record format's fields: the physical file's when none are named.
/// Columns are written `{prefix}"column"`; `collate` is the COLLATE clause
/// character values computed from other columns get.
pub fn columns(
    lf: &LogicalFile,
    pf: &[FieldDef],
    pf_name: &str,
    prefix: &str,
    collate: &str,
) -> Result<Vec<Column>> {
    if lf.fields.is_empty() {
        return Ok(pf.iter().map(|f| as_is(f, &f.name, 'B', prefix)).collect());
    }
    build(&lf.fields, pf, pf_name, prefix, collate)
}

/// A physical file field as it is, under its name or another (RENAME).
fn as_is(src: &FieldDef, name: &str, usage: char, prefix: &str) -> Column {
    let mut def = src.clone();
    let renamed = name != src.name;
    if renamed {
        def.name = name.to_string();
        def.alias = None;
        def.reference = None;
        def.identity = None;
        def.generated = None;
    }
    let set_by_db = src.generated.is_some() || src.identity.as_ref().is_some_and(|i| i.always);
    Column {
        read: format!("{prefix}{}", src.column()),
        plain: true,
        writes: if usage == 'B' && !set_by_db {
            vec![(src.column(), format!("NEW.{}", def.column()))]
        } else {
            vec![]
        },
        params: if renamed {
            vec![src.name.clone()]
        } else {
            vec![]
        },
        default: view_default(src, &def).ok().flatten(),
        def,
        usage,
    }
}

fn build(
    fields: &[LfField],
    pf: &[FieldDef],
    pf_name: &str,
    prefix: &str,
    collate: &str,
) -> Result<Vec<Column>> {
    // A record format with a concatenation cannot hold fields that allow
    // the null value (DDS reference, CONCAT).
    let has_concat = fields
        .iter()
        .any(|f| matches!(f.source, LfSource::Concat(_)));
    let mut out: Vec<Column> = Vec::new();
    for f in fields {
        let col = match &f.source {
            LfSource::Field(src) => field(f, pf_field(pf, src, pf_name)?, prefix, collate)?,
            LfSource::Concat(parts) => {
                let parts = parts
                    .iter()
                    .map(|p| pf_field(pf, p, pf_name))
                    .collect::<Result<Vec<_>>>()?;
                concat(f, &parts, prefix, collate)?
            }
            LfSource::Sst {
                field,
                start,
                length,
            } => {
                // A field defined earlier in the logical file, else the
                // physical file's (DDS reference, SST).
                let earlier = fields.iter().position(|x| &x.name == field);
                let (src, expr) = match earlier.and_then(|i| out.get(i)) {
                    Some(c) => {
                        if !matches!(fields[earlier.unwrap_or(0)].source, LfSource::Field(_)) {
                            bail!(
                                "field {}: the field named on SST cannot be a CONCAT or SST field",
                                f.name
                            );
                        }
                        (c.def.clone(), c.read.clone())
                    }
                    None => {
                        let p = pf_field(pf, field, pf_name)?;
                        (p.clone(), format!("{prefix}{}", p.column()))
                    }
                };
                sst(f, &src, &expr, *start, *length, collate)?
            }
        };
        if has_concat && col.def.null_capable {
            bail!(
                "field {}: a record format with a CONCAT field cannot include fields that allow the null value",
                f.name
            );
        }
        out.push(col);
    }
    Ok(out)
}

/// A field from one physical file field: as it is, renamed, or with a
/// length, data type or decimal positions of its own.
fn field(f: &LfField, src: &FieldDef, prefix: &str, collate: &str) -> Result<Column> {
    let name = &f.name;
    if f.length.is_none() && f.data_type.is_none() && f.decimals.is_none() && !f.varlen {
        let mut c = as_is(src, name, f.usage, prefix);
        if let Some(t) = &f.text {
            c.def.text = t.clone();
        }
        return Ok(c);
    }
    let ty = f.data_type.unwrap_or(src.data_type);
    if src.data_type.is_lob() || ty.is_lob() {
        bail!(
            "field {name}: {} fields are not supported in logical files",
            src.data_type.code()
        );
    }
    let numeric = ty.is_numeric();
    // Decimal positions: a value, or a change of the physical field's
    // (DDS reference, positions 36-37 for logical files).
    let decimals = match f.decimals {
        None if numeric && src.data_type.is_numeric() => src.decimals,
        None => 0,
        Some(_) if !numeric => {
            bail!("field {name}: decimal positions are only valid for numeric fields")
        }
        Some(_) if src.data_type == DataType::Binary && src.decimals > 0 => bail!(
            "field {name}: the decimal positions of a binary field with decimal positions cannot be changed"
        ),
        Some(Decimals::Set(n)) => n,
        Some(Decimals::Change(c)) => {
            let n = src.decimals as i32 + c;
            if n < 0 {
                bail!("field {name}: decimal positions {c:+} leave fewer than none");
            }
            n as u32
        }
    };
    // A float shown as a fixed-point number needs its length and decimal
    // positions given (DDS reference, conversion from floating point).
    if src.data_type == DataType::Float
        && numeric
        && ty != DataType::Float
        && (f.length.is_none() || f.decimals.is_none())
    {
        bail!(
            "field {name}: a floating-point field shown as {} needs a length and decimal positions",
            ty.code()
        );
    }
    let length = match (f.length, ty.fixed_length()) {
        (Some(_), Some(_)) if ty == src.data_type => {
            bail!(
                "field {name}: a {} field has no length of its own",
                ty.code()
            )
        }
        (Some(n), _) => n,
        (None, Some(n)) => n,
        (None, None) => src.length,
    };
    if numeric {
        if length == 0 || length > 63 || decimals > length {
            bail!("field {name}: invalid numeric size {length},{decimals}");
        }
        if ty == DataType::Binary && length > 18 {
            bail!("field {name}: binary fields have 1 to 18 digits");
        }
        if ty == DataType::Binary && src.data_type != DataType::Binary && decimals != 0 {
            bail!("field {name}: a binary field over a field that is not binary has no decimal positions");
        }
    } else if length == 0 || length > 32766 {
        bail!("field {name}: invalid length {length}");
    }
    let mut def = FieldDef::new(name, ty, length, decimals);
    def.text = f.text.clone().unwrap_or_else(|| src.text.clone());
    def.varlen = matches!(ty, DataType::Char | DataType::Graphic | DataType::Hex)
        && if f.length.is_none() {
            src.varlen
        } else {
            f.varlen
        };
    def.null_capable = src.null_capable;
    def.double = ty == DataType::Float && src.double;
    def.keywords = src.keywords.clone();
    let renamed = name != &src.name;
    let same = def.sql_type() == src.sql_type() && def.varlen == src.varlen;
    if !renamed {
        def.alias = src.alias.clone();
        def.reference = src.reference.clone();
    }
    if same {
        def.default = src.default.clone();
        def.hidden = src.hidden;
        if !renamed {
            def.identity = src.identity.clone();
            def.generated = src.generated;
        }
    }
    check_conversion(src, &def)?;
    let column = format!("{prefix}{}", src.column());
    let plain = same;
    let read = if plain {
        column
    } else {
        with_collation(&def, convert(src, &def, &column)?, collate)
    };
    let new = format!("NEW.{}", def.column());
    let set_by_db = src.generated.is_some() || src.identity.as_ref().is_some_and(|i| i.always);
    let writes = if f.usage == 'B' && !set_by_db {
        vec![(
            src.column(),
            if plain {
                new
            } else {
                convert(&def, src, &new)?
            },
        )]
    } else {
        vec![]
    };
    let default = view_default(src, &def)?;
    Ok(Column {
        def,
        usage: f.usage,
        read,
        plain,
        writes,
        params: if renamed {
            vec![src.name.clone()]
        } else {
            vec![]
        },
        default,
    })
}

/// The view column's default: the physical field's, in the logical
/// field's type (a record written through the logical file without the
/// field gets it).
fn view_default(src: &FieldDef, def: &FieldDef) -> Result<Option<String>> {
    let clause = src.default_clause();
    let Some(e) = clause.strip_prefix(" DEFAULT ") else {
        return Ok(None);
    };
    if e == "NULL" {
        return Ok(Some("NULL".into()));
    }
    Ok(Some(
        if def.sql_type() == src.sql_type() && def.varlen == src.varlen {
            e.to_string()
        } else {
            convert(src, def, &format!("({e})"))?
        },
    ))
}

/// Which changes of data type a logical file may make (DDS reference,
/// data type for physical and logical files, the table of valid
/// conversions and its notes).
fn check_conversion(pf: &FieldDef, lf: &FieldDef) -> Result<()> {
    use DataType::*;
    let (a, b) = (pf.data_type, lf.data_type);
    let invalid = || {
        anyhow!(
            "field {}: a {} field cannot be shown as {} in a logical file",
            lf.name,
            a.code(),
            b.code()
        )
    };
    let not_yet = || {
        anyhow!(
            "field {}: showing a {} field as {} in a logical file is not supported yet",
            lf.name,
            a.code(),
            b.code()
        )
    };
    // Note 2: valid only if the binary field has no decimal positions.
    let binary_ok = (a != Binary || pf.decimals == 0) && (b != Binary || lf.decimals == 0);
    match (a, b) {
        (Char, Char)
        | (Hex, Hex)
        | (Date, Date)
        | (Time, Time)
        | (Timestamp, Timestamp)
        | (Graphic, Graphic) => Ok(()),
        // Note 3: binary to binary keeps the decimal positions.
        (Binary, Binary) if pf.decimals == lf.decimals => Ok(()),
        (Binary, Binary) => Err(invalid()),
        (Packed | Zoned | Float, Packed | Zoned | Float) => Ok(()),
        (Packed | Zoned | Float | Binary, Packed | Zoned | Float | Binary) if binary_ok => Ok(()),
        (Packed | Zoned | Float | Binary, Packed | Zoned | Float | Binary) => Err(invalid()),
        // Note 1: as many characters as digits, and not variable length.
        (Char, Zoned) | (Zoned, Char) => {
            let (chars, digits, varlen) = if a == Char {
                (pf.length, lf.length, pf.varlen)
            } else {
                (lf.length, pf.length, lf.varlen)
            };
            if chars != digits || varlen {
                bail!(
                    "field {}: a character field and a zoned decimal field over each other need as many characters as digits and a fixed length",
                    lf.name
                );
            }
            Ok(())
        }
        (Char, Hex) | (Hex, Char) | (Hex, Zoned) | (Zoned, Hex) => Err(not_yet()),
        (Date, Char | Hex | Zoned) | (Time, Zoned) | (Timestamp, Date | Time) => Err(not_yet()),
        (Graphic, _) | (_, Graphic) => Err(not_yet()),
        _ => Err(invalid()),
    }
}

/// A value of `from`'s type as `to`'s. Decimal positions are cut, not
/// rounded (DDS reference, decimal positions for logical files: 2.52 read
/// with no decimal positions is 2); a value too large for `to` is an
/// error.
fn convert(from: &FieldDef, to: &FieldDef, e: &str) -> Result<String> {
    use DataType::*;
    Ok(match (from.data_type, to.data_type) {
        (_, Float) => format!("CAST({e} AS {})", to.sql_type()),
        (Packed | Zoned | Binary | Float, Packed | Zoned | Binary) => format!(
            "CAST(trunc(CAST({e} AS numeric), {}) AS {})",
            to.decimals,
            to.sql_type()
        ),
        (Zoned, Char) => format!(
            "CAST(as4tainr_svc.zoned_chars(CAST({e} AS numeric), {}, {}) AS {})",
            from.length,
            from.decimals,
            to.sql_type()
        ),
        (Char, Zoned) => format!(
            "CAST(as4tainr_svc.chars_zoned(rpad(CAST({e} AS text), {}), {}, false) AS {})",
            from.length,
            to.decimals,
            to.sql_type()
        ),
        (a, b) if a == b => format!("CAST({e} AS {})", to.sql_type()),
        (a, b) => bail!(
            "field {}: converting {} to {} is not supported yet",
            to.name,
            a.code(),
            b.code()
        ),
    })
}

fn with_collation(def: &FieldDef, e: String, collate: &str) -> String {
    if matches!(def.data_type, DataType::Char | DataType::Graphic) {
        format!("({e}){collate}")
    } else {
        e
    }
}

/// A part of a concatenation as characters: a character field padded to
/// its length, a numeric field as its zoned decimal digits.
fn chars_of(f: &FieldDef, e: &str) -> String {
    if f.data_type.is_numeric() {
        format!(
            "as4tainr_svc.zoned_chars(CAST({e} AS numeric), {}, 0)",
            f.length
        )
    } else if f.varlen {
        format!("CAST({e} AS text)")
    } else {
        format!("rpad(CAST({e} AS text), {})", f.length)
    }
}

/// CONCAT (DDS reference): the fields one after another. Any character
/// field makes a character field, only numeric fields a zoned decimal
/// field of their digits with the sign of the rightmost; the length is
/// the sum of theirs.
fn concat(f: &LfField, parts: &[&FieldDef], prefix: &str, collate: &str) -> Result<Column> {
    use DataType::*;
    let name = &f.name;
    for p in parts {
        match p.data_type {
            Float | Date | Time | Timestamp => bail!(
                "field {name}: a {} field ({}) cannot be part of a concatenation",
                p.data_type.code(),
                p.name
            ),
            Packed | Zoned | Binary | Char if !p.data_type.is_numeric() || p.decimals == 0 => {}
            Packed | Zoned | Binary => bail!(
                "field {name}: field {} has decimal positions and cannot be part of a concatenation",
                p.name
            ),
            other => bail!(
                "field {name}: concatenating {} fields ({}) is not supported yet",
                other.code(),
                p.name
            ),
        }
    }
    let character = parts.iter().any(|p| p.data_type == Char);
    let length: u32 = parts.iter().map(|p| p.length).sum();
    let varlen = f.varlen || parts.iter().any(|p| p.varlen);
    let null = parts.iter().any(|p| p.null_capable);
    if character && length > 32766 || !character && length > 63 {
        bail!("field {name}: the concatenation is {length} long, more than its data type allows");
    }
    if let Some(l) = f.length {
        if l != length {
            bail!(
                "field {name}: the length of a CONCAT field is the sum of its fields' ({length})"
            );
        }
    }
    // A variable-length or null-capable concatenation is input only.
    if (varlen || null) && f.usage != 'I' {
        bail!("field {name}: a variable-length or null-capable CONCAT field must be input only (I in position 38)");
    }
    let ty = if character { Char } else { Zoned };
    let mut def = FieldDef::new(name, ty, length, 0);
    def.varlen = varlen;
    def.null_capable = null;
    def.text = f.text.clone().unwrap_or_default();
    let col = |p: &FieldDef| format!("{prefix}{}", p.column());
    let read = if character {
        let joined = parts
            .iter()
            .map(|p| chars_of(p, &col(p)))
            .collect::<Vec<_>>()
            .join(" || ");
        with_collation(
            &def,
            format!("CAST({joined} AS {})", def.sql_type()),
            collate,
        )
    } else {
        let digits = parts
            .iter()
            .map(|p| {
                format!(
                    "as4tainr_svc.digits(CAST({} AS numeric), {})",
                    col(p),
                    p.length
                )
            })
            .collect::<Vec<_>>()
            .join(" || ");
        let last = col(parts[parts.len() - 1]);
        format!(
            "CAST(CASE WHEN {last} < 0 THEN -1 ELSE 1 END * CAST({digits} AS numeric) AS {})",
            def.sql_type()
        )
    };
    // Written back: each field from its place in the value.
    let mut writes = Vec::new();
    if f.usage == 'B' {
        let new = format!("NEW.{}", def.column());
        let mut at = 1;
        for (i, p) in parts.iter().enumerate() {
            let value = if character {
                let piece = format!(
                    "substr(rpad(CAST({new} AS text), {length}), {at}, {})",
                    p.length
                );
                if p.data_type.is_numeric() {
                    format!(
                        "CAST(as4tainr_svc.chars_zoned({piece}, 0, true) AS {})",
                        p.sql_type()
                    )
                } else {
                    format!("CAST({piece} AS {})", p.sql_type())
                }
            } else {
                let piece = format!(
                    "CAST(substr(as4tainr_svc.digits(CAST({new} AS numeric), {length}), {at}, {}) AS numeric)",
                    p.length
                );
                // The sign is the rightmost field's.
                let signed = if i == parts.len() - 1 {
                    format!("CASE WHEN {new} < 0 THEN -{piece} ELSE {piece} END")
                } else {
                    piece
                };
                format!("CAST({signed} AS {})", p.sql_type())
            };
            writes.push((p.column(), value));
            at += p.length;
        }
    }
    Ok(Column {
        def,
        usage: f.usage,
        read,
        plain: false,
        writes,
        params: parts.iter().map(|p| p.name.clone()).collect(),
        default: None,
    })
}

/// SST (DDS reference): part of a character, hexadecimal or zoned field;
/// a character field (a zoned field's part is its digits as characters),
/// input only.
fn sst(
    f: &LfField,
    src: &FieldDef,
    e: &str,
    start: u32,
    length: Option<u32>,
    collate: &str,
) -> Result<Column> {
    use DataType::*;
    let name = &f.name;
    let Some(length) = length else {
        bail!("field {name}: SST needs a length, on the keyword or in positions 30-34");
    };
    let chars = match src.data_type {
        Char => {
            if src.varlen {
                format!("CAST({e} AS text)")
            } else {
                format!("rpad(CAST({e} AS text), {})", src.length)
            }
        }
        Zoned => format!(
            "as4tainr_svc.zoned_chars(CAST({e} AS numeric), {}, {})",
            src.length, src.decimals
        ),
        Hex | Graphic => bail!(
            "field {name}: a substring of a {} field is not supported yet",
            src.data_type.code()
        ),
        other => bail!(
            "field {name}: SST needs a character, hexadecimal, zoned or graphic field, not {}",
            other.code()
        ),
    };
    if start + length - 1 > src.length {
        bail!(
            "field {name}: the substring is longer than field {} ({})",
            src.name,
            src.length
        );
    }
    if f.data_type.is_some_and(|t| t != Char) {
        bail!(
            "field {name}: a substring of a {} field is a character field",
            src.data_type.code()
        );
    }
    if f.decimals.is_some() {
        bail!("field {name}: an SST field has no decimal positions");
    }
    let mut def = FieldDef::new(name, Char, length, 0);
    def.null_capable = src.null_capable;
    def.text = f.text.clone().unwrap_or_default();
    let read = with_collation(
        &def,
        format!(
            "CAST(substr({chars}, {start}, {length}) AS {})",
            def.sql_type()
        ),
        collate,
    );
    Ok(Column {
        def,
        usage: 'I',
        read,
        plain: false,
        writes: vec![],
        params: vec![],
        default: None,
    })
}

/// The expression a key or select/omit field name stands for: a field of
/// the record format, else a physical file field named on a RENAME or
/// CONCAT keyword (DDS reference, select/omit field name: "Fields
/// specified in DDS positions 19 through 28", then "Fields specified as
/// parameters on the CONCAT or RENAME keyword").
pub fn find<'a>(
    cols: &'a [Column],
    pf: &'a [FieldDef],
    name: &str,
    prefix: &str,
) -> Option<(FieldDef, String, bool)> {
    if let Some(c) = cols.iter().find(|c| c.def.name == name) {
        return Some((c.def.clone(), c.read.clone(), true));
    }
    if cols.iter().any(|c| c.params.iter().any(|p| p == name)) {
        let f = pf.iter().find(|f| f.name == name)?;
        return Some((f.clone(), format!("{prefix}{}", f.column()), false));
    }
    None
}

/// The column a format field's name has in the view.
pub fn view_column(c: &Column) -> String {
    c.def.column()
}

/// Does changing records through the view need the trigger: a field
/// converted, concatenated or input only, or one physical field set by
/// two fields?
pub fn needs_trigger(cols: &[Column]) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    for c in cols {
        if !c.plain || c.usage != 'B' {
            return true;
        }
        for (pc, _) in &c.writes {
            if seen.contains(&pc.as_str()) {
                return true;
            }
            seen.push(pc);
        }
    }
    false
}

/// The trigger function's body: INSERT, UPDATE and DELETE through the
/// view change the physical file's table. Fields are moved in their
/// order, so the last field that sets a physical field gives its value
/// (DDS reference, RENAME); input-only fields are left as they are on an
/// update and take their defaults on an insert.
pub fn trigger_body(cols: &[Column], table: &str) -> String {
    let mut sets: Vec<(String, String)> = Vec::new();
    for c in cols {
        for (pc, e) in &c.writes {
            sets.retain(|(x, _)| x != pc);
            sets.push((pc.clone(), e.clone()));
        }
    }
    let insert = if sets.is_empty() {
        format!("INSERT INTO {table} DEFAULT VALUES RETURNING rrn INTO NEW.rrn;")
    } else {
        format!(
            "INSERT INTO {table} ({}) VALUES ({}) RETURNING rrn INTO NEW.rrn;",
            sets.iter()
                .map(|(c, _)| c.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            sets.iter()
                .map(|(_, e)| e.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let update = if sets.is_empty() {
        format!("PERFORM 1 FROM {table} WHERE rrn = OLD.rrn;")
    } else {
        format!(
            "UPDATE {table} SET {} WHERE rrn = OLD.rrn;",
            sets.iter()
                .map(|(c, e)| format!("{c} = {e}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "BEGIN
  IF TG_OP = 'INSERT' THEN
    {insert}
    RETURN NEW;
  ELSIF TG_OP = 'UPDATE' THEN
    {update}
    IF NOT FOUND THEN RETURN NULL; END IF;
    RETURN NEW;
  ELSE
    DELETE FROM {table} WHERE rrn = OLD.rrn;
    IF NOT FOUND THEN RETURN NULL; END IF;
    RETURN OLD;
  END IF;
END"
    )
}

/// The trigger function's name in the logical file's library.
pub fn trigger_function(lib: &str, name: &str) -> String {
    format!("{}.{}", ident(lib), ident(&format!("{name}~lfw")))
}
