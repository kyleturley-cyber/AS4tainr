//! Exceptions and errors (ILE RPG Reference, "Exception/Error Handling",
//! "Program Status Codes", "File Status Codes"): the status code each
//! error stands for; the program status data structure (PSDS) and the
//! file information data structures (INFDS); the program error
//! subroutine *PSSR and the file error subroutines (INFSR).
//!
//! An error an operation does not handle itself (E extender, error
//! indicator in positions 73-74) and no MONITOR group handles goes to
//! the procedure's *PSSR (or, for a file error in the main procedure, the
//! file's INFSR subroutine). The subroutine ends with a GOTO back into the
//! calculations, a RETURN, or ENDSR: with a return point (cycle main
//! procedure: *DETL, *GETIN, *TOTC, *TOTL, *OFL, *DETC, *CANCL) the cycle
//! goes on there; with none, the error goes to the default handler (the
//! program ends abnormally), and a subprocedure fails.

use super::*;
use crate::rpg::program::FileInfo;

/// Most nested calls of an error subroutine for errors in itself, before
/// the error goes to the default handler (on IBM i the subroutine is
/// called again each time; a program that loops there never ends).
const MAX_HANDLER_DEPTH: u32 = 20;

/// Where an error a procedure did not handle happened: the statement's
/// line and the call stack entry (shown as "line N").
#[derive(Debug, Clone)]
pub struct At {
    pub line: usize,
    pub sender: crate::pgmmsg::Sender,
}

impl std::fmt::Display for At {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}", self.line)
    }
}

/// Where an error happened (each `At` added on the way out carries the
/// first one's sender).
pub(super) fn origin(err: &anyhow::Error) -> Option<At> {
    err.downcast_ref::<At>().cloned()
}

/// The message identifier an error starts with (`CPF5026: ...`), if any.
///
/// A run-time error as4tainr reports as an ILE RPG inquiry message
/// (RNQxxxx) is signaled first as the escape message of the same number,
/// RNXxxxx (ILE RPG Programmer's Guide, "ILE RPG Exception Handling":
/// the RNX escape message is what MONITOR, *PSSR and the program status
/// data structure see; the RNQ inquiry is sent only when nothing handles
/// it). The halt indicators (RNQ0231-0233) are no exception.
pub(super) fn message_id(err: &anyhow::Error) -> String {
    let id = crate::jobs::escape_id(&err.root_cause().to_string()).to_string();
    match id.strip_prefix("RNQ") {
        Some(n) if !matches!(n, "0231" | "0232" | "0233") => format!("RNX{n}"),
        _ => id,
    }
}

/// The RPG status code of an error (ILE RPG Reference, "Program Status
/// Codes" and "File Status Codes"). Program status codes are 00100-00999,
/// file status codes 01000-09999 (ON-ERROR *PROGRAM and *FILE).
pub(super) fn status_code(err: &anyhow::Error) -> u32 {
    let text = err.root_cause().to_string();
    // A called program that ended in error: 00202.
    if text.trim_end().ends_with("ended abnormally.") {
        // 00231: it ended with a halt indicator on.
        if text.contains("RNQ0232") || text.contains("RNQ0233") {
            return 231;
        }
        return 202;
    }
    let id = message_id(err);
    match id.as_str() {
        // Divide by zero; a procedure or program that cannot be called
        // (not found, signature not valid).
        "MCH1211" => 102,
        "MCH4431" | "CPF4101" => 211,
        // A pointer that addresses nothing, or storage outside what it
        // addresses: pointer or parameter error.
        "MCH3601" | "MCH0601" => 222,
        // I/O to a file that is not open; OPEN of a file already open;
        // an implicit open that failed.
        "CPF4135" => 1211,
        "CPF4161" => 1215,
        "CPF9812" | "CPF4326" => 1216,
        // Record in use; duplicate key; a constraint not satisfied;
        // UPDATE or DELETE without a record read before.
        "CPF5027" => 1218,
        "CPF5026" => 1021,
        // Check and referential constraints: 01022.
        "CPF502F" | "CPF502D" => 1022,
        // Error in a trigger program (ILE RPG Reference, file status
        // codes): 01023 before the operation, 01024 after it.
        "CPF502B" if text.contains("(before the change)") => 1023,
        "CPF502B" => 1024,
        "CPF5013" if text.contains("requires a prior") => 1221,
        // Data area not found.
        "CPF1015" => 401,
        _ if id.starts_with("RNQ") || id.starts_with("RNX") => {
            id[3..].parse::<u32>().unwrap_or(9999)
        }
        // Other I/O errors.
        _ if id.starts_with("CPF5") || id.starts_with("CPD5") || id.starts_with("CPF4") => 1299,
        _ => 9999,
    }
}

/// Whether an ON-ERROR list entry takes a status code: *ALL, *PROGRAM
/// (00100-00999), *FILE (01000-09999), or the code itself.
pub(super) fn on_error_takes(entry: &str, code: u32) -> bool {
    match entry {
        "*ALL" => true,
        "*PROGRAM" => code < 1000,
        "*FILE" => code >= 1000,
        n => n.parse::<u32>().is_ok_and(|n| n == code),
    }
}

/// How an error subroutine ended: a GOTO to a TAG, or a RETURN.
pub(super) enum Recovery {
    Goto(String),
    Return(Option<Value>),
}

/// Control signals that pass through error handling unchanged.
pub(super) fn is_signal(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ReturnPointSignal>().is_some()
        || err.downcast_ref::<GotoSignal>().is_some()
        || err.downcast_ref::<ReturnSignal>().is_some()
}

/// The operation name a file information data structure shows: five
/// characters (ILE RPG Reference, "File Feedback Information", *OPCODE).
fn opcode_name(op: FileOp) -> &'static str {
    match op {
        FileOp::Read => "READ",
        FileOp::ReadE => "READE",
        FileOp::ReadP => "READP",
        FileOp::ReadPE => "REDPE",
        FileOp::Chain => "CHAIN",
        FileOp::SetLL => "SETLL",
        FileOp::SetGT => "SETGT",
        FileOp::Write => "WRITE",
        FileOp::Update => "UPDAT",
        FileOp::Delete => "DELET",
        FileOp::Open => "OPEN",
        FileOp::Close => "CLOSE",
        FileOp::Unlock => "UNLCK",
        FileOp::Feod => "FEOD",
        FileOp::Exfmt => "EXFMT",
        FileOp::ReadC => "READC",
    }
}

/// The attention identifier byte of a key (5250 data stream).
fn aid_byte(aid: &str) -> u8 {
    match aid {
        "ENTER" => 0xF1,
        "ROLLDOWN" => 0xF4,
        "ROLLUP" => 0xF5,
        "HELP" => 0xF3,
        "CLEAR" => 0xBD,
        a => match a.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
            Some(n @ 1..=12) => 0x30 + n,
            Some(n @ 13..=24) => 0xB0 + n - 12,
            _ => a
                .strip_prefix("AID")
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .unwrap_or(0),
        },
    }
}

impl<'a> Machine<'a> {
    // ---- storage of the status data structures -----------------------

    /// Write bytes into a global data structure, at an offset; what falls
    /// past its end is not kept.
    fn ds_put(&mut self, ds: &str, off: usize, bytes: &[u8]) {
        let Some(Sym::Ds(d)) = self.prog.syms.get(ds) else {
            return;
        };
        if off >= d.size {
            return;
        }
        let n = bytes.len().min(d.size - off);
        let at = d.off + off;
        if let Some(b) = self.bufs.get_mut(d.buf) {
            if at + n <= b.len() {
                b[at..at + n].copy_from_slice(&bytes[..n]);
            }
        }
    }

    fn ds_get(&self, ds: &str, off: usize, len: usize) -> Vec<u8> {
        let Some(Sym::Ds(d)) = self.prog.syms.get(ds) else {
            return vec![];
        };
        let end = (off + len).min(d.size);
        if off >= end {
            return vec![];
        }
        self.bufs
            .get(d.buf)
            .and_then(|b| b.get(d.off + off..d.off + end))
            .map(|s| s.to_vec())
            .unwrap_or_default()
    }

    fn ds_text(&mut self, ds: &str, from: usize, len: usize, text: &str) {
        let mut b = data::e(text);
        b.resize(len, data::BLANK);
        b.truncate(len);
        self.ds_put(ds, from - 1, &b);
    }

    fn ds_zoned(&mut self, ds: &str, from: usize, digits: u32, v: i64) {
        let t = DType::Zoned(digits, 0);
        let mut b = t.initial();
        let v = v.rem_euclid(10i64.pow(digits));
        if data::store(
            &t,
            &Value::Num(Dec::from_i64(v)),
            &mut b,
            Fit::Truncate { half_adjust: false },
            false,
        )
        .is_ok()
        {
            self.ds_put(ds, from - 1, &b);
        }
    }

    fn ds_bin2(&mut self, ds: &str, from: usize, v: i64) {
        self.ds_put(ds, from - 1, &(v as i16).to_be_bytes());
    }

    fn ds_bin4(&mut self, ds: &str, from: usize, v: i64) {
        self.ds_put(ds, from - 1, &(v as i32).to_be_bytes());
    }

    /// The current statement number as the status data structures show
    /// it: eight positions.
    fn statement(&self) -> String {
        format!("{:>8}", self.line)
    }

    // ---- the program status data structure ----------------------------

    /// What the PSDS holds from the start of a call (ILE RPG Reference,
    /// "Program Status Data Structure"): *PROC, *PARMS, the program's
    /// library, the job, the dates, the program and module, the user.
    pub(crate) fn psds_start(&mut self) {
        let Some(ds) = self.prog.psds.clone() else {
            return;
        };
        let proc_name = self
            .prog
            .main
            .clone()
            .unwrap_or_else(|| self.prog.name.clone());
        self.ds_text(&ds, 1, 10, &proc_name);
        self.ds_zoned(&ds, 37, 3, self.parms_passed as i64);
        // A module of a service program: the service program; one bound
        // into a program: that program.
        let (lib, pgm) = match (self.current_srvpgm(), self.io.program.split_once('/')) {
            (Some(x), _) => x,
            (None, Some(("*BOUND", _))) => (self.stack_lib(), self.stack_pgm()),
            (None, Some((l, p))) if l != "*NONE" => (l.to_string(), p.to_string()),
            _ => (String::new(), self.prog.name.clone()),
        };
        self.ds_text(&ds, 81, 10, &lib);
        let (y, m, d) = today();
        let (hh, mi, ss) = now_hms();
        let yy = (y % 100) as i64;
        let mdy = (m as i64) * 10_000 + (d as i64) * 100 + yy;
        // The date the job entered the system (*DATE form) and its
        // century; the job's date, the run date and time.
        self.ds_text(&ds, 191, 8, &format!("{m:02}{d:02}{y:04}"));
        self.ds_zoned(&ds, 199, 2, (y / 100) as i64);
        if let Some(job) = self.io.job.clone() {
            self.ds_text(&ds, 244, 10, &job.name);
            self.ds_text(&ds, 254, 10, &job.user);
            self.ds_zoned(&ds, 264, 6, job.number);
        }
        self.ds_zoned(&ds, 270, 6, mdy);
        self.ds_zoned(&ds, 276, 6, mdy);
        self.ds_zoned(&ds, 282, 6, (hh * 10_000 + mi * 100 + ss) as i64);
        // When the program was created and from which source member.
        if !lib.is_empty() {
            let row = self.io.rt.clone().block_on(self.io.client.query_opt(
                "SELECT to_char(o.created, 'MMDDYY'), to_char(o.created, 'HH24MISS'),
                                p.src_file, p.src_lib, p.src_mbr
                           FROM as4tainr.programs p JOIN as4tainr.objects o
                             ON o.library = p.library AND o.name = p.name AND o.objtype = '*PGM'
                          WHERE p.library = $1 AND p.name = $2",
                &[&lib, &pgm],
            ));
            if let Ok(Some(r)) = row {
                let g = |i: usize| r.get::<_, String>(i);
                self.ds_text(&ds, 288, 6, &g(0));
                self.ds_text(&ds, 294, 6, &g(1));
                self.ds_text(&ds, 304, 10, &g(2));
                self.ds_text(&ds, 314, 10, &g(3));
                self.ds_text(&ds, 324, 10, &g(4));
            }
        }
        // The program and module the procedure is in; the current user.
        self.ds_text(&ds, 334, 10, &pgm);
        self.ds_text(&ds, 344, 10, &self.prog.name.clone());
        let user = self.io.user.clone();
        self.ds_text(&ds, 358, 10, &user);
    }

    /// An exception in the program: its status code, the one before it,
    /// the statement, routine, message identifier and data (PSDS 11-15,
    /// 16-20, 21-28, 29-36, 40-46, 91-170). A file error also shows the
    /// file and its status information (175-184, 201-208, 209-243).
    pub(super) fn psds_error(&mut self, code: u32, err: &anyhow::Error) {
        let Some(ds) = self.prog.psds.clone() else {
            return;
        };
        let prev = self.ds_get(&ds, 11, 5);
        if prev.len() == 5 {
            self.ds_put(&ds, 15, &prev);
        }
        if code < 1000 {
            self.ds_zoned(&ds, 11, 5, code as i64);
        }
        let stmt = self.statement();
        self.ds_text(&ds, 21, 8, &stmt);
        let routine = self.routine.clone();
        self.ds_text(&ds, 29, 8, &routine);
        let id = message_id(err);
        self.ds_text(&ds, 40, 7, &id);
        let root = err.root_cause().to_string();
        let data = root
            .split_once(": ")
            .map(|(_, t)| t.to_string())
            .unwrap_or(root);
        self.ds_text(&ds, 91, 80, data.lines().next().unwrap_or(""));
        if code >= 1000 {
            if let Some(i) = self.err_file {
                let name = self.prog.files[i].decl.name.clone();
                self.ds_text(&ds, 175, 10, &name);
                self.ds_text(&ds, 201, 8, &name);
                if let Some(info) = self.infds_feedback(i) {
                    self.ds_put(&ds, 208, &info);
                }
            }
        }
    }

    /// The file status information PSDS 209-243 shows: INFDS positions
    /// 11-45 as they would be.
    fn infds_feedback(&self, i: usize) -> Option<Vec<u8>> {
        let st = &self.files[i].status;
        let mut b = Vec::with_capacity(35);
        let mut z = DType::Zoned(5, 0).initial();
        data::store(
            &DType::Zoned(5, 0),
            &Value::Num(Dec::from_i64(st.code as i64)),
            &mut z,
            Fit::Truncate { half_adjust: false },
            false,
        )
        .ok()?;
        b.extend(z);
        let mut text = |s: &str, n: usize| {
            let mut x = data::e(s);
            x.resize(n, data::BLANK);
            x.truncate(n);
            b.extend(x);
        };
        text(&st.opcode, 6);
        text(&self.routine, 8);
        text(&self.statement(), 8);
        text(&st.record, 8);
        Some(b)
    }

    // ---- the file information data structure ---------------------------

    /// The INFDS of a file, if it has one.
    fn infds_of(&self, i: usize) -> Option<String> {
        keyword(&self.prog.files[i].decl.keywords, "INFDS")
            .and_then(|k| k.args.first())
            .map(|a| a.trim().to_ascii_uppercase())
    }

    /// After an operation on a file: its file feedback (INFDS 1-80), its
    /// I/O feedback (241-366) and device feedback (367 on).
    pub(super) fn infds_update(&mut self, i: usize) {
        let Some(ds) = self.infds_of(i) else {
            return;
        };
        let name = self.prog.files[i].decl.name.clone();
        let open = self.files[i].open.is_some() || self.files[i].ws_open;
        let st = self.files[i].status.clone();
        self.ds_text(&ds, 1, 8, &name);
        self.ds_text(&ds, 9, 1, if open { "1" } else { "0" });
        self.ds_text(&ds, 10, 1, if st.eof { "1" } else { "0" });
        self.ds_zoned(&ds, 11, 5, st.code as i64);
        self.ds_text(&ds, 16, 6, &st.opcode);
        let routine = self.routine.clone();
        self.ds_text(&ds, 22, 8, &routine);
        let stmt = self.statement();
        self.ds_text(&ds, 30, 8, &stmt);
        self.ds_text(&ds, 38, 8, &st.record);
        self.ds_text(&ds, 46, 7, &st.msgid);
        // I/O feedback: counts, the record format, the record length.
        let f = &self.files[i];
        let (w, r, o) = (f.writes, f.reads, f.others);
        let rrn = f.open.as_ref().and_then(|o| o.current_rrn());
        let aid = f.aid.clone();
        self.ds_bin4(&ds, 243, w);
        self.ds_bin4(&ds, 247, r);
        self.ds_bin4(&ds, 251, 0);
        self.ds_bin4(&ds, 255, o);
        let fmt = self.prog.files[i].format.clone();
        self.ds_text(&ds, 261, 10, &fmt);
        let len = match self.prog.files[i].record_len {
            0 => self.prog.files[i].decl.record_len.unwrap_or(0) as i64,
            n => n as i64,
        };
        self.ds_bin4(&ds, 283, len);
        match self.prog.files[i].decl.device.as_str() {
            // Database: the relative record number of the record.
            "DISK" => self.ds_bin4(&ds, 397, rrn.unwrap_or(0)),
            // Printer: the current line and page.
            "PRINTER" => {
                let (line, page) = self
                    .printers
                    .get(&name)
                    .map(|p| (p.line as i64, p.page as i64))
                    .unwrap_or((1, 1));
                self.ds_bin2(&ds, 367, line);
                self.ds_bin4(&ds, 369, page);
            }
            // Display: the attention identifier of the key pressed.
            "WORKSTN" => {
                if let Some(a) = aid {
                    self.ds_put(&ds, 368, &[aid_byte(&a)]);
                }
            }
            _ => {}
        }
    }

    /// When a file opens: its open feedback (INFDS 81-240).
    pub(super) fn infds_open(&mut self, i: usize) {
        let Some(ds) = self.infds_of(i) else {
            return;
        };
        let decl = self.prog.files[i].decl.clone();
        let (odp, rows, cols) = match decl.device.as_str() {
            "PRINTER" => ("SP", 66, decl.record_len.unwrap_or(132) as i64),
            "WORKSTN" => ("DS", 24, 80),
            _ => ("DB", 0, 0),
        };
        self.ds_text(&ds, 81, 2, odp);
        let (lib, file) = match &self.prog.files[i].fref {
            Some(fr) => (fr.library.clone(), fr.file.name.clone()),
            None => match &self.prog.files[i].display {
                Some(d) => (d.library.clone(), d.file.name.clone()),
                None => (String::new(), decl.name.clone()),
            },
        };
        self.ds_text(&ds, 83, 10, &file);
        self.ds_text(&ds, 93, 10, &lib);
        if let Some(fr) = self.prog.files[i].fref.clone() {
            let len = self.prog.files[i].record_len as i64;
            let keylen = self.prog.files[i].key_len as i64;
            self.ds_bin2(&ds, 125, len);
            self.ds_bin2(&ds, 127, keylen);
            self.ds_text(&ds, 129, 10, &fr.file.name);
            // The number of records in the member when it opened.
            let table = format!(
                "{}.{}",
                crate::catalog::ident(&fr.library),
                crate::catalog::ident(&fr.file.name)
            );
            let n: i64 = self
                .block(
                    self.io
                        .client
                        .query_one(&format!("SELECT count(*) FROM {table}"), &[]),
                )
                .map(|r| r.get(0))
                .unwrap_or(0);
            self.ds_bin4(&ds, 156, n);
            let keyed = !fr.file.keys.is_empty() && decl.keyed;
            self.ds_text(&ds, 160, 2, if keyed { "KU" } else { "AR" });
            self.ds_text(&ds, 162, 1, if fr.file.unique { "U" } else { "D" });
            self.ds_text(&ds, 163, 1, "N");
        }
        if rows > 0 {
            self.ds_bin2(&ds, 152, rows);
            self.ds_bin2(&ds, 154, cols);
        }
        if decl.device == "PRINTER" {
            let ofl = keyword(&decl.keywords, "FORMOFL")
                .and_then(|k| k.args.first())
                .and_then(|a| a.trim().parse::<i64>().ok())
                .unwrap_or(crate::spool::OVERFLOW_LINE as i64);
            self.ds_bin2(&ds, 188, ofl);
        }
    }

    // ---- status after operations ---------------------------------------

    /// A file operation failed: its status code and message (an error the
    /// E extender or the error indicator takes).
    pub(super) fn file_failed(&mut self, i: usize, err: &anyhow::Error) {
        let code = status_code(err);
        let st = &mut self.files[i].status;
        st.error = true;
        st.code = code;
        st.msgid = message_id(err);
        self.err_file = Some(i);
        self.last = Status {
            error: true,
            code,
            ..Status::default()
        };
    }

    /// A file operation: the operation itself, then the file's status
    /// (00000; 00011 end of file; 00012 no record found; an error's
    /// code), %STATUS, and the file information data structure.
    pub(super) fn file_op(
        &mut self,
        op: FileOp,
        file: &str,
        key: &Option<Vec<Expr>>,
        ext: &Ext,
        ind: &ResultInd,
    ) -> Result<()> {
        let i = self
            .file_idx(file)
            .ok_or_else(|| anyhow!("{file} is not a file of this program"))?;
        // E: %ERROR and %STATUS start over for the operation.
        if ext.error {
            self.last = Status::default();
        }
        self.files[i].status.error = false;
        // A file under commitment control: the operation runs in a
        // savepoint of the unit of work, so that one that fails takes
        // back only its own changes.
        let cmt = self.files[i].cmt.clone();
        if let Some(d) = &cmt {
            self.block(async {
                d.client
                    .batch_execute("SAVEPOINT as4tainr_rla")
                    .await
                    .map_err(anyhow::Error::from)
            })?;
        }
        let r = self.file_op_inner(op, file, key, ext, ind);
        if let Some(d) = &cmt {
            self.block(async {
                if d.client
                    .batch_execute("RELEASE SAVEPOINT as4tainr_rla")
                    .await
                    .is_err()
                {
                    d.client
                        .batch_execute("ROLLBACK TO SAVEPOINT as4tainr_rla")
                        .await?;
                }
                Ok::<(), anyhow::Error>(())
            })?;
        }
        let by_name = self.prog.files[i].decl.name == file;
        let record = if self.prog.files[i].fref.is_some() || self.prog.files[i].display.is_some() {
            if by_name {
                self.prog.files[i].format.clone()
            } else {
                file.to_string()
            }
        } else {
            String::new()
        };
        match op {
            FileOp::Write => self.files[i].writes += 1,
            FileOp::Read
            | FileOp::ReadE
            | FileOp::ReadP
            | FileOp::ReadPE
            | FileOp::Chain
            | FileOp::ReadC
            | FileOp::Exfmt => self.files[i].reads += 1,
            _ => self.files[i].others += 1,
        }
        match &r {
            Err(err) if !is_signal(err) => self.file_failed(i, err),
            Err(_) => {}
            Ok(()) if self.files[i].status.error => {}
            Ok(()) => {
                let st = &self.files[i].status;
                let code = match op {
                    FileOp::Read
                    | FileOp::ReadE
                    | FileOp::ReadP
                    | FileOp::ReadPE
                    | FileOp::ReadC
                        if st.eof =>
                    {
                        11
                    }
                    FileOp::Chain | FileOp::SetLL | FileOp::SetGT if !st.found => 12,
                    _ => 0,
                };
                self.files[i].status.code = code;
                self.files[i].status.msgid.clear();
                self.last.code = code;
            }
        }
        let st = &mut self.files[i].status;
        st.opcode = format!("{:<5}{}", opcode_name(op), if by_name { 'F' } else { 'R' });
        st.record = record;
        self.infds_update(i);
        r
    }

    /// The file a name in the calculations refers to: one declared in the
    /// running procedure first, then the program's.
    pub(super) fn file_idx(&self, name: &str) -> Option<usize> {
        // A file parameter (LIKEFILE): the caller's file.
        if let Some(i) = self.frames.last().and_then(|f| f.files.get(name)) {
            return Some(*i);
        }
        let files = &self.prog.files;
        let proc = self.frames.last().map(|f| f.proc.name.as_str());
        let local = |f: &FileInfo| f.decl.proc.as_deref() == proc;
        files
            .iter()
            .position(|f| local(f) && f.decl.name == name)
            .or_else(|| files.iter().position(|f| local(f) && f.format == name))
            .or_else(|| {
                files.iter().position(|f| {
                    f.decl.proc.is_none() && (f.decl.name == name || f.format == name)
                })
            })
            .or_else(|| self.prog.file_index(name))
    }

    /// A statement, with the E extender (or an error indicator in
    /// positions 73-74 of IN, OUT and CALLP): an error sets %ERROR and
    /// %STATUS and the program goes on.
    pub(super) fn stmt_e(&mut self, s: &Stmt) -> Result<()> {
        let (e, lo) = match s {
            Stmt::Eval { ext, .. } | Stmt::Arith { ext, .. } => (ext.error, None),
            Stmt::Op { code, ext, ind, .. } => (
                ext.error,
                ind.lo
                    .clone()
                    .filter(|_| matches!(code.as_str(), "IN" | "OUT" | "CALLP")),
            ),
            _ => (false, None),
        };
        if !e && lo.is_none() {
            return self.stmt(s);
        }
        if e {
            self.last = Status::default();
        }
        if let Some(l) = &lo {
            self.set_ind(l, false);
        }
        match self.stmt(s) {
            Err(err) if !is_signal(&err) && !self.passes_by(&err) => {
                self.note_error(&err);
                if let Some(l) = &lo {
                    self.set_ind(l, true);
                }
                Ok(())
            }
            r => r,
        }
    }

    // ---- files declared in procedures -----------------------------------

    /// A procedure's own files open when it is called (ILE RPG
    /// Reference, "File Considerations"): those without USROPN; a STATIC
    /// file stays open between calls.
    pub(super) fn open_local_files(&mut self, pi: &ProcInfo) -> Result<()> {
        for i in 0..self.prog.files.len() {
            let d = &self.prog.files[i].decl;
            if d.proc.as_deref() != Some(pi.name.as_str())
                || keyword(&d.keywords, "USROPN").is_some()
                || keyword(&d.keywords, "TEMPLATE").is_some()
                || self.files[i].open.is_some()
                || self.files[i].ws_open
            {
                continue;
            }
            self.open_file(i)?;
        }
        Ok(())
    }

    /// When the procedure returns: its files close, except STATIC ones.
    pub(super) fn close_local_files(&mut self, pi: &ProcInfo) {
        for i in 0..self.prog.files.len() {
            let d = &self.prog.files[i].decl;
            if d.proc.as_deref() != Some(pi.name.as_str())
                || keyword(&d.keywords, "STATIC").is_some()
            {
                continue;
            }
            if let Some(f) = self.files[i].open.take() {
                let _ = self.close_open_file(f);
            }
            self.files[i].ws_open = false;
        }
    }

    // ---- error subroutines ---------------------------------------------

    /// An error that reached the procedure: its status in the PSDS and
    /// %STATUS, then the error subroutine that takes it: the file's INFSR
    /// for a file error in the main procedure, else the procedure's
    /// *PSSR.
    fn handler_for(&self, code: u32) -> Option<(String, &'a Code)> {
        let prog: &'a Program = self.prog;
        match self.frames.last() {
            Some(f) => {
                let p: &'a ProcInfo = f.proc;
                p.routines
                    .subroutines
                    .get("*PSSR")
                    .map(|c| ("*PSSR".to_string(), c))
            }
            None => {
                if code >= 1000 {
                    if let Some(i) = self.err_file {
                        let sr = keyword(&prog.files[i].decl.keywords, "INFSR")
                            .and_then(|k| k.args.first())
                            .map(|a| a.trim().to_ascii_uppercase());
                        if let Some(sr) = sr {
                            if let Some(c) = prog.routines.subroutines.get(&sr) {
                                return Some((sr, c));
                            }
                        }
                    }
                }
                prog.routines
                    .subroutines
                    .get("*PSSR")
                    .map(|c| ("*PSSR".to_string(), c))
            }
        }
    }

    /// An escape message sent to a caller of this procedure: no handler
    /// here takes it (QMHSNDPM *ESCAPE ends the entries above its
    /// target).
    pub(super) fn passes_by(&self, err: &anyhow::Error) -> bool {
        err.downcast_ref::<crate::pgmmsg::Escape>()
            .is_some_and(|e| {
                e.passes(
                    &self.io.job_key,
                    crate::pgmmsg::stack_top_id(&self.io.job_key),
                )
            })
    }

    /// Record an error as the program sees it: %STATUS, %ERROR stay as
    /// the operation left them; the PSDS shows the exception.
    pub(super) fn note_error(&mut self, err: &anyhow::Error) -> u32 {
        if std::env::var("AS4TAINR_TRACE_ERRORS").is_ok() {
            eprintln!("[handled in {}] {err:#}", self.prog.name);
        }
        let code = status_code(err);
        if code < 1000 {
            self.last = Status {
                error: true,
                code,
                ..Status::default()
            };
        } else {
            self.last.code = code;
            self.last.error = true;
        }
        self.psds_error(code, err);
        // The exception message goes to the call message queue of the
        // procedure that handled it (QMHRCVPM *EXCP finds it there).
        if let Some(id) = crate::pgmmsg::stack_target(&self.io.job_key, "*", 0) {
            let (m, logged) = match err.downcast_ref::<crate::pgmmsg::Escape>() {
                // In the job log already where it was sent (an entry
                // procedure): not again.
                Some(e) => (e.0.clone(), e.0.key != 0),
                None => {
                    let msgid = message_id(err);
                    let mut m = crate::pgmmsg::from_error(err, &msgid);
                    m.data = super::data::e(&m.text);
                    // Sent by the procedure where it happened.
                    let at = origin(err)
                        .map(|a| a.sender)
                        .unwrap_or_else(|| self.sender());
                    // The job log shows an RPG run-time error sent by the
                    // run time (QRNXIE) to the procedure, at the statement
                    // where it happened (PUB400 2026-10-28,
                    // tools/golden/verify21.txt h21_joblog); QMHRCVPM still
                    // gives the procedure as its sender (docs/fidelity-gaps.md).
                    m.receiver.line = at.line;
                    if msgid.starts_with("RNX") {
                        m.logged_sender = Some(crate::pgmmsg::rpg_runtime_sender());
                    }
                    m.sender = at;
                    (m, false)
                }
            };
            if logged {
                crate::pgmmsg::stack_send_unlogged(&self.io.job_key, id, m);
            } else if let Some(k) = crate::pgmmsg::stack_send(&self.io.job_key, id, m) {
                self.logged.push(k);
            }
        }
        code
    }

    /// An error no operation and no MONITOR group took: run the error
    /// subroutine. Ok: how it ended (GOTO, RETURN); Err: the error goes
    /// on (no subroutine, ENDSR without a return point, a return point
    /// for the cycle).
    pub(super) fn recover(&mut self, err: anyhow::Error) -> Result<Recovery> {
        if is_signal(&err) || self.passes_by(&err) {
            return Err(err);
        }
        let code = self.note_error(&err);
        let Some((name, sub)) = self.handler_for(code) else {
            return Err(err);
        };
        if self.in_handler >= MAX_HANDLER_DEPTH {
            return Err(err);
        }
        self.in_handler += 1;
        let saved = std::mem::replace(&mut self.routine, name);
        let r = self.run_code(sub);
        self.routine = saved;
        self.in_handler -= 1;
        match r {
            Ok(Flow::Return(v)) => Ok(Recovery::Return(v)),
            // ENDSR without a return point: the default error handler.
            Ok(_) => Err(err),
            Err(e) => {
                if let Some(GotoSignal(t)) = e.downcast_ref::<GotoSignal>() {
                    return Ok(Recovery::Goto(t.clone()));
                }
                if e.downcast_ref::<ReturnPointSignal>().is_some() {
                    return Err(e);
                }
                // An error in the subroutine itself goes to it again.
                self.recover(e)
            }
        }
    }

    /// Run a procedure's calculations with its error subroutine: after a
    /// GOTO from it, on at the TAG.
    pub(super) fn guarded(&mut self, code: &Code) -> Result<Flow> {
        let mut start = 0;
        loop {
            let err = match self.run_code_at(code, start) {
                Ok(f) => return Ok(f),
                Err(e) => e,
            };
            match self.recover(err)? {
                Recovery::Return(v) => return Ok(Flow::Return(v)),
                Recovery::Goto(t) => {
                    match code
                        .iter()
                        .position(|x| matches!(&x.ins, Ins::Label(l) if *l == t))
                    {
                        Some(at) => start = at,
                        None => return Err(GotoSignal(t).into()),
                    }
                }
            }
        }
    }
}

/// The time now (UTC): hours, minutes, seconds.
fn now_hms() -> (u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = (secs % 86_400) as u32;
    (s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Status codes from the ILE RPG Reference tables "Program Status
    // Codes" (00102 divide by zero, 00121 array index, 00907 decimal
    // data, 00202 called program failed) and "File Status Codes" (01021
    // duplicate key, 01211 file not open, 01218 record locked).
    #[test]
    fn status_codes() {
        let c = |t: &str| status_code(&anyhow!("{t}"));
        assert_eq!(c("MCH1211: Attempt to divide by zero."), 102);
        assert_eq!(c("RNQ0121: An array index is out of range."), 121);
        assert_eq!(c("RNQ0907: Decimal-data error."), 907);
        assert_eq!(c("RNQ0102: x\nProgram X ended abnormally."), 202);
        assert_eq!(c("CPF5026: duplicate key"), 1021);
        assert_eq!(c("CPF4135: File F is not open."), 1211);
        assert_eq!(c("CPF5027: Record 3 in file F is in use."), 1218);
        assert!(on_error_takes("*PROGRAM", 102) && !on_error_takes("*FILE", 102));
        assert!(on_error_takes("*FILE", 1211) && on_error_takes("01211", 1211));
        assert!(on_error_takes("1211", 1211) && !on_error_takes("1218", 1211));
    }

    #[test]
    fn aid_bytes() {
        // 5250 attention identifiers: Enter F1, F1-F12 31-3C, F13-F24
        // B1-BC.
        assert_eq!(aid_byte("ENTER"), 0xF1);
        assert_eq!(aid_byte("F3"), 0x33);
        assert_eq!(aid_byte("F24"), 0xBC);
    }
}
