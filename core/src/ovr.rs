//! File overrides (OVRDBF, OVRPRTF, DLTOVR).
//!
//! Written from IBM's public CL documentation and the file management
//! topics on overrides. An override replaces the file a program opens
//! (TOFILE) or some of its attributes when the program opens it. Overrides
//! belong to the call level that made them (OVRSCOPE(*ACTGRPDFN) in the
//! default activation group): they stay in effect for the programs that
//! level calls and end when the program that made them returns, or with
//! DLTOVR. When several call levels override the same file, the override
//! made at the higher call level (nearer the start of the job) is the one
//! that counts; a second override of a file at the same level replaces the
//! first. From a program of a non-default activation group,
//! OVRSCOPE(*ACTGRPDFN) scopes the override to the activation group
//! instead, and DLTOVR LVL(*) deletes that activation group's overrides.

/// What kind of file an override is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Db,
    Printer,
}

#[derive(Debug, Clone)]
pub struct Override {
    /// The call level that made it (0: the command line).
    pub level: usize,
    pub kind: Kind,
    /// The file the programs name.
    pub file: String,
    /// The file to open instead (`LIB/NAME`, `*LIBL/NAME`).
    pub tofile: Option<String>,
    /// OVRPRTF PAGESIZE (length, width) and OVRFLW.
    pub pagesize: Option<(i32, i32)>,
    pub ovrflw: Option<i32>,
    /// OVRDBF LVLCHK(*NO): no record format level check.
    pub no_lvlchk: bool,
    /// OVRSCOPE(*ACTGRPDFN) from a program of a non-default activation
    /// group: the override is scoped to that activation group, so it
    /// stays when the program that made it returns.
    pub actgrp: Option<String>,
}

/// The override in effect for a file: the one of the highest call level.
pub fn effective<'a>(ovr: &'a [Override], kind: Kind, file: &str) -> Option<&'a Override> {
    let name = file.rsplit('/').next().unwrap_or(file);
    ovr.iter()
        .filter(|o| o.kind == kind && o.file.eq_ignore_ascii_case(name))
        .min_by_key(|o| o.level)
}

/// The name to look a file up by: TOFILE of the override in effect, or
/// the name as the program wrote it.
pub fn file_name(ovr: &[Override], kind: Kind, file: &str) -> String {
    match effective(ovr, kind, file).and_then(|o| o.tofile.clone()) {
        Some(t) => t.strip_prefix("*LIBL/").unwrap_or(&t).to_string(),
        None => file.to_string(),
    }
}

/// Overrides scoped to activation groups, by job: they outlive the
/// program that made them, so they are kept for the job rather than with
/// a program's call level.
static SCOPED: std::sync::Mutex<Option<std::collections::HashMap<String, Vec<Override>>>> =
    std::sync::Mutex::new(None);

fn with_scoped<R>(f: impl FnOnce(&mut std::collections::HashMap<String, Vec<Override>>) -> R) -> R {
    let mut g = SCOPED.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Default::default))
}

/// The job's activation-group overrides, added to `ovr` where missing.
pub fn merge_scoped(job: &str, ovr: &mut Vec<Override>) {
    with_scoped(|m| {
        for o in m.get(job).into_iter().flatten() {
            if !ovr
                .iter()
                .any(|x| x.actgrp == o.actgrp && x.kind == o.kind && x.file == o.file)
            {
                ovr.push(o.clone());
            }
        }
    })
}

/// Keep the activation-group overrides of `ovr` as the job's.
pub fn save_scoped(job: &str, ovr: &[Override]) {
    let scoped: Vec<Override> = ovr.iter().filter(|o| o.actgrp.is_some()).cloned().collect();
    with_scoped(|m| {
        m.insert(job.to_string(), scoped);
    })
}

/// Activation groups end: their overrides go.
pub fn end_groups(job: &str, ends: impl Fn(&str) -> bool) {
    with_scoped(|m| {
        if let Some(v) = m.get_mut(job) {
            v.retain(|o| !o.actgrp.as_deref().is_some_and(&ends));
        }
    })
}

/// The job ends.
pub fn end_job(job: &str) {
    with_scoped(|m| {
        m.remove(job);
    })
}

/// Add an override at a call level (replacing one of the same file made
/// at that level).
pub fn add(ovr: &mut Vec<Override>, o: Override) {
    ovr.retain(|x| {
        !(x.level == o.level && x.actgrp == o.actgrp && x.kind == o.kind && x.file == o.file)
    });
    ovr.push(o);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(level: usize, file: &str, to: &str) -> Override {
        Override {
            level,
            kind: Kind::Db,
            file: file.into(),
            tofile: Some(to.into()),
            pagesize: None,
            ovrflw: None,
            no_lvlchk: false,
            actgrp: None,
        }
    }

    // Overrides at several call levels: the highest level (the earliest
    // in the call stack) counts; the same level replaces.
    #[test]
    fn precedence() {
        let mut v = Vec::new();
        add(&mut v, o(2, "CUST", "LIB2/C2"));
        add(&mut v, o(1, "CUST", "LIB1/C1"));
        assert_eq!(file_name(&v, Kind::Db, "CUST"), "LIB1/C1");
        assert_eq!(file_name(&v, Kind::Db, "MYLIB/CUST"), "LIB1/C1");
        add(&mut v, o(1, "CUST", "*LIBL/C3"));
        assert_eq!(file_name(&v, Kind::Db, "CUST"), "C3");
        assert_eq!(file_name(&v, Kind::Printer, "CUST"), "CUST");
        assert_eq!(file_name(&v, Kind::Db, "OTHER"), "OTHER");
    }
}
