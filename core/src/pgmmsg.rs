//! Program message queues: the messages CL programs send each other.
//!
//! Written from IBM's public CL documentation (SNDPGMMSG, RCVMSG, RTVMSG,
//! MONMSG) and the message handling topics. Every program running in a
//! job has its own program message queue. SNDPGMMSG puts a message on
//! the queue of the program itself (TOPGMQ(*SAME)) or of its caller
//! (TOPGMQ(*PRV), the default); an *ESCAPE message to the caller ends the
//! sending program and is an exception in the caller, which a MONMSG can
//! handle. RCVMSG takes messages off the program's own queue: new
//! messages by type, first in first out, except *EXCP, which receives the
//! last exception; RMV(*NO) keeps a received message as an old message,
//! which only *FIRST, *LAST, *NEXT, *PRV or its key receive again.
//!
//! The queues live in the job ([`CallStack`]): one per CL program
//! running, innermost last.

use anyhow::{bail, Result};
use std::collections::HashMap;
use tokio_postgres::Client;

/// A message on a program message queue.
#[derive(Debug, Clone, Default)]
pub struct QMsg {
    pub key: u32,
    /// Blank for an impromptu message (SNDPGMMSG MSG).
    pub msgid: String,
    pub msgf: String,
    pub msgflib: String,
    /// The library the sender named (*LIBL when it did not name one).
    pub sndmsgflib: String,
    /// Message data, as stored (EBCDIC).
    pub data: Vec<u8>,
    pub text: String,
    pub seclvl: String,
    pub sev: i32,
    /// *INFO, *COMP, *DIAG, *ESCAPE, *NOTIFY, *INQ, *RQS, *STATUS.
    pub ty: String,
    /// Received with RMV(*NO).
    pub old: bool,
    /// The program, module and procedure that sent it (RPG programs).
    pub sender: Sender,
    /// When it was sent: CYYMMDD and HHMMSS.
    pub sent: (String, String),
    /// And the microseconds (6 digits).
    pub usec: String,
}

#[derive(Debug, Default)]
pub struct CallStack {
    /// One program message queue per CL program running, innermost last.
    stack: Vec<Vec<QMsg>>,
    /// The escape message being raised, for the caller's MONMSG and
    /// RCVMSG MSGTYPE(*EXCP).
    pub escape: Option<QMsg>,
    /// The escape was sent to the caller: the sender's own MONMSGs do
    /// not see it.
    pub escape_to_caller: bool,
    next_key: u32,
    /// The text system messages had in this job, by identifier: system
    /// message files have no descriptions here, so a message resent
    /// with the identifier and data RCVMSG returned keeps its text.
    texts: HashMap<String, String>,
    /// File overrides (OVRDBF, OVRPRTF), with the call level of each.
    pub overrides: Vec<crate::ovr::Override>,
    /// The parameters' values when the program CALL ran ended:
    /// parameters are passed by reference, so the caller's variables get
    /// them back.
    pub returned: Option<Vec<String>>,
    /// The printed output (a compiler listing) of the last command, for
    /// the `system` utility to show.
    pub printed: Vec<String>,
    /// The message data of the last command's completion message, when
    /// it has some (CRTSRVPGM: the service program and its library).
    pub message_data: Option<Vec<u8>>,
}

impl CallStack {
    /// A program starts: its queue.
    pub fn enter(&mut self) {
        self.stack.push(Vec::new());
    }

    /// A program ends: its queue and the overrides it made go with it.
    pub fn leave(&mut self) {
        let level = self.stack.len();
        self.overrides
            .retain(|o| o.level < level || o.actgrp.is_some());
        self.stack.pop();
    }

    /// The current call level: 0 on the command line, 1 in a CL program
    /// called from it, and so on.
    pub fn level(&self) -> usize {
        self.stack.len()
    }

    fn key(&mut self) -> u32 {
        self.next_key = self.next_key.wrapping_add(1).max(1);
        self.next_key
    }

    /// Put a message on the running program's queue (`caller`: its
    /// caller's); returns the message key. A program called from the
    /// command line has no CL caller: the message is only shown.
    pub fn put(&mut self, mut m: QMsg, caller: bool) -> u32 {
        let k = self.key();
        m.key = k;
        if !m.msgid.is_empty() && !m.text.is_empty() {
            self.texts.insert(m.msgid.clone(), m.text.clone());
        }
        let n = self.stack.len();
        let at = if caller {
            n.checked_sub(2)
        } else {
            n.checked_sub(1)
        };
        if let Some(q) = at.and_then(|i| self.stack.get_mut(i)) {
            q.push(m);
        }
        k
    }

    /// The text a system message had in this job.
    pub fn known_text(&self, msgid: &str) -> Option<&String> {
        self.texts.get(msgid)
    }

    /// RCVMSG on the running program's queue: the message, removed
    /// (`rmv`) or kept as old.
    pub fn receive(&mut self, msgtype: &str, key: Option<u32>, rmv: &str) -> Option<QMsg> {
        self.receive_from(0, msgtype, key, rmv)
    }

    /// RCVMSG PGMQ: the queue `up` levels above the running program's
    /// (1: its caller's, PGMQ(*PRV)).
    pub fn receive_from(
        &mut self,
        up: usize,
        msgtype: &str,
        key: Option<u32>,
        rmv: &str,
    ) -> Option<QMsg> {
        let n = self.stack.len().checked_sub(1 + up)?;
        let q = self.stack.get_mut(n)?;
        let i = select(q, msgtype, key)?;
        let exception = matches!(q[i].ty.as_str(), "*ESCAPE" | "*NOTIFY");
        let remove = match rmv {
            "*NO" => false,
            "*KEEPEXCP" => !exception,
            _ => true,
        };
        if remove {
            Some(q.remove(i))
        } else {
            q[i].old = true;
            Some(q[i].clone())
        }
    }
}

/// Which message RCVMSG receives (RCVMSG MSGTYPE and MSGKEY).
pub fn select(q: &[QMsg], msgtype: &str, key: Option<u32>) -> Option<usize> {
    let pos = key.map(|k| q.iter().position(|m| m.key == k));
    let new = |ty: &[&str]| q.iter().position(|m| !m.old && ty.contains(&m.ty.as_str()));
    match (msgtype, pos) {
        // A key that is not on the queue: nothing.
        (_, Some(None)) => None,
        ("*NEXT", Some(Some(p))) => (p + 1 < q.len()).then_some(p + 1),
        ("*PRV", Some(Some(p))) => p.checked_sub(1),
        ("*ANY", Some(Some(p))) => Some(p),
        ("*EXCP", Some(Some(p))) => matches!(q[p].ty.as_str(), "*ESCAPE" | "*NOTIFY").then_some(p),
        (t, Some(Some(p))) => (q[p].ty == t).then_some(p),
        ("*FIRST" | "*NEXT", None) => (!q.is_empty()).then_some(0),
        ("*LAST" | "*PRV", None) => q.len().checked_sub(1),
        ("*ANY", None) => q.iter().position(|m| !m.old),
        // The last exception (last in, first out).
        ("*EXCP", None) => q
            .iter()
            .rposition(|m| !m.old && matches!(m.ty.as_str(), "*ESCAPE" | "*NOTIFY")),
        (t, None) => new(&[t]),
    }
}

/// RCVMSG RTNTYPE: the two-digit code of a message type.
pub fn rtntype(ty: &str) -> &'static str {
    match ty {
        "*COMP" => "01",
        "*DIAG" => "02",
        "*INFO" => "04",
        "*INQ" => "05",
        "*COPY" => "06",
        "*RQS" => "08",
        "*NOTIFY" => "14",
        "*ESCAPE" => "15",
        _ => "",
    }
}

/// A message key as RCVMSG KEYVAR and SNDPGMMSG KEYVAR return it: four
/// characters (printable here; see docs/fidelity-gaps.md).
pub fn key_text(k: u32) -> String {
    const D: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut n = k % 36u32.pow(4);
    let mut out = [b'0'; 4];
    for c in out.iter_mut().rev() {
        *c = D[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_key(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.len() != 4 {
        return None;
    }
    s.chars()
        .try_fold(0u32, |acc, c| c.to_digit(36).map(|d| acc * 36 + d))
}

/// The exception message for an error a program caught: its identifier
/// and its text (without the identifier).
pub fn from_error(e: &anyhow::Error, id: &str) -> QMsg {
    let s = format!("{e:#}");
    let line = s
        .lines()
        .find(|l| l.starts_with(id))
        .or_else(|| s.lines().next())
        .unwrap_or("");
    let text = line
        .strip_prefix(id)
        .map(|t| t.trim_start_matches(':').trim())
        .unwrap_or(line)
        .to_string();
    QMsg {
        msgid: id.to_string(),
        msgf: crate::msgf::system_msgf(id).into(),
        msgflib: "QSYS".into(),
        sndmsgflib: "*LIBL".into(),
        text,
        sev: if id == "CPF9898" { 40 } else { 0 },
        ty: "*ESCAPE".into(),
        ..Default::default()
    }
}

/// The message files of the system: they have no descriptions here.
fn is_system_msgf(name: &str) -> bool {
    matches!(
        name,
        "QCPFMSG" | "QRNXMSG" | "QSQLMSG" | "QCPFMSGL" | "QCPFMSGU"
    )
}

/// The text of a predefined message: from its message description (MSGF
/// qualified `LIB/NAME` or just `NAME`, found through the library list),
/// with the message data substituted. A system message file has no
/// descriptions here: the text is the message data (CPF9898, whose text
/// is its data, with a period), or the text the message had when this job
/// last saw it.
pub async fn describe(
    client: &Client,
    libs: &[String],
    state: &CallStack,
    msgid: &str,
    msgf: &str,
    data: &[u8],
) -> Result<QMsg> {
    let msgid = crate::msgf::check_msgid(msgid)?;
    let msgf = msgf.trim().trim_matches('\'').to_ascii_uppercase();
    let (lib, name) = match msgf.split_once('/') {
        Some((l, n)) => (l.to_string(), n.to_string()),
        None => ("*LIBL".to_string(), msgf.clone()),
    };
    let search = if lib == "*LIBL" {
        libs.to_vec()
    } else {
        vec![lib.clone()]
    };
    let mut m = QMsg {
        msgid: msgid.clone(),
        msgf: name.clone(),
        sndmsgflib: lib.clone(),
        data: data.to_vec(),
        ..Default::default()
    };
    match crate::msgf::locate(client, &search, &name).await? {
        Some(found) => {
            let Some(d) =
                crate::msgf::find(client, std::slice::from_ref(&found), &name, &msgid).await?
            else {
                bail!("CPF2419: Message {msgid} is not in message file {name} in library {found}.");
            };
            let (text, seclvl) = d.format(data);
            m.msgflib = found;
            m.text = text;
            m.seclvl = seclvl;
            m.sev = d.sev;
        }
        None if is_system_msgf(&name) && (lib == "*LIBL" || lib == "QSYS") => {
            let given = crate::rpg::data::a(data).trim_end().to_string();
            m.msgflib = "QSYS".into();
            m.text = match (msgid.as_str(), state.known_text(&msgid)) {
                ("CPF9898", _) if !given.is_empty() => {
                    if given.ends_with('.') {
                        given
                    } else {
                        format!("{given}.")
                    }
                }
                (_, Some(t)) if given.is_empty() || data.iter().all(|b| *b == 0x40) => t.clone(),
                _ if !given.is_empty() => given,
                _ => format!("Message {msgid} from {name}."),
            };
            m.sev = if msgid == "CPF9898" { 40 } else { 0 };
        }
        None => bail!("CPF2407: Message file {name} in {lib} not found."),
    }
    Ok(m)
}

/// Program message queues by call stack entry name, for the message
/// APIs (QMHSNDPM, QMHRMVPM) and message subfiles (SFLPGMQ): each job's
/// queues, by the name of the program or procedure they belong to.
type Named = HashMap<(String, String), Vec<QMsg>>;
static NAMED: std::sync::Mutex<Option<Named>> = std::sync::Mutex::new(None);
static NAMED_KEY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn queue_name(q: &str) -> String {
    q.trim().to_ascii_uppercase()
}

/// Put a message on a named program message queue; its key.
pub fn named_send(job: &str, queue: &str, mut m: QMsg) -> u32 {
    let key = NAMED_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    m.key = key;
    if let Ok(mut g) = NAMED.lock() {
        g.get_or_insert_with(HashMap::new)
            .entry((job.to_string(), queue_name(queue)))
            .or_default()
            .push(m);
    }
    key
}

/// QMHRMVPM messages to remove: *ALL, *BYKEY (the message with `key`),
/// *OLD, *NEW, *KEEPRQS (all but requests).
pub fn named_remove(job: &str, queue: &str, which: &str, key: Option<u32>) {
    if let Ok(mut g) = NAMED.lock() {
        if let Some(q) = g
            .as_mut()
            .and_then(|m| m.get_mut(&(job.to_string(), queue_name(queue))))
        {
            match which {
                "*BYKEY" => q.retain(|m| Some(m.key) != key),
                "*OLD" => q.retain(|m| !m.old),
                "*NEW" => q.retain(|m| m.old),
                "*KEEPRQS" => q.retain(|m| m.ty == "*RQS"),
                _ => q.clear(),
            }
        }
    }
}

/// The messages on a named program message queue, oldest first.
pub fn named_messages(job: &str, queue: &str) -> Vec<QMsg> {
    NAMED
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .and_then(|m| m.get(&(job.to_string(), queue_name(queue))).cloned())
        })
        .unwrap_or_default()
}

/// One call stack entry of the ILE programs a job runs (a program's main
/// procedure, or a procedure), innermost last, with its call message
/// queue (ILE Concepts, "Call stack"; the message handling APIs address
/// queues by call stack entry and counter).
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub id: u64,
    /// The program or service program the procedure is in.
    pub pgm: String,
    pub module: String,
    pub proc: String,
    /// Its activation group: a change marks a control boundary.
    pub actgrp: String,
    pub queue: Vec<QMsg>,
    /// The library of the program or service program.
    pub lib: String,
    /// The source line the procedure is running (where it called the
    /// next entry); 0 when not known.
    pub line: usize,
}

/// The job log as the list APIs see it (QGYOLJBL): every message sent
/// to a call stack entry, oldest first.
static JOBLOG: std::sync::Mutex<Option<HashMap<String, Vec<QMsg>>>> = std::sync::Mutex::new(None);

/// The job's logged messages, oldest first.
pub fn job_log(job: &str) -> Vec<QMsg> {
    JOBLOG
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(job).cloned()))
        .unwrap_or_default()
}

static STACKS: std::sync::Mutex<Option<HashMap<String, Vec<Entry>>>> = std::sync::Mutex::new(None);
static ENTRY_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn with_stack<R>(job: &str, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    let mut g = STACKS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new)
        .entry(job.to_string())
        .or_default())
}

/// A procedure starts: its call stack entry.
pub fn stack_push(
    job: &str,
    (lib, pgm): (&str, &str),
    module: &str,
    proc: &str,
    actgrp: &str,
) -> u64 {
    let id = ENTRY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    with_stack(job, |s| {
        s.push(Entry {
            id,
            lib: lib.to_string(),
            line: 0,
            pgm: pgm.to_string(),
            module: module.to_string(),
            proc: proc.to_string(),
            actgrp: actgrp.to_string(),
            queue: Vec::new(),
        })
    });
    id
}

/// A procedure ended: its entry (and any above it) and their queues go.
pub fn stack_pop(job: &str, id: u64) {
    with_stack(job, |s| {
        if let Some(i) = s.iter().position(|e| e.id == id) {
            s.truncate(i);
        }
    });
}

/// The entries, innermost last.
pub fn stack(job: &str) -> Vec<Entry> {
    with_stack(job, |s| s.clone())
}

/// The program of the innermost entry.
/// The activation group of the running procedure (None: no RPG
/// procedure is running).
pub fn stack_actgrp(job: &str) -> Option<String> {
    with_stack(job, |s| s.last().map(|e| e.actgrp.clone()))
}

/// The running procedure is at a source line (it calls another).
pub fn stack_line(job: &str, line: usize) {
    with_stack(job, |s| {
        if let Some(e) = s.last_mut() {
            e.line = line;
        }
    })
}

pub fn stack_top_pgm(job: &str) -> Option<String> {
    with_stack(job, |s| s.last().map(|e| e.pgm.clone()))
}

/// The entry a message API names (QMHSNDPM, QMHRCVPM call stack entry
/// and counter): `*` the current one, `*PGMBDY` the first entry of the
/// current program, `*CTLBDY` the first of the current activation group,
/// or a procedure or program name (the innermost such entry); then
/// `counter` entries toward the caller. None: an entry above the RPG
/// programs (a CL program, a command line).
pub fn stack_target(job: &str, entry: &str, counter: usize) -> Option<u64> {
    with_stack(job, |s| {
        let top = s.len().checked_sub(1)?;
        let e = entry.trim().to_ascii_uppercase();
        let base = match e.as_str() {
            "*" | "" => top,
            "*PGMBDY" => {
                let pgm = &s[top].pgm;
                let mut i = top;
                while i > 0 && s[i - 1].pgm == *pgm {
                    i -= 1;
                }
                i
            }
            "*CTLBDY" => {
                let g = &s[top].actgrp;
                let mut i = top;
                while i > 0 && s[i - 1].actgrp == *g {
                    i -= 1;
                }
                i
            }
            // Procedure names as exported ('assert', or in capitals);
            // matched ignoring case (fidelity-gaps: QMHSNDPM entry names).
            name => s.iter().rposition(|x| {
                x.proc.eq_ignore_ascii_case(name) || x.pgm.eq_ignore_ascii_case(name)
            })?,
        };
        base.checked_sub(counter).map(|i| s[i].id)
    })
}

/// Put a message on an entry's queue; its key.
pub fn stack_send(job: &str, id: u64, mut m: QMsg) -> Option<u32> {
    let key = NAMED_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    m.key = key;
    if m.sent.0.is_empty() {
        let ((y, mo, d), (h, mi, sec), us) = crate::rpg::dates::now();
        m.sent = (
            format!("{}{:02}{mo:02}{d:02}", u8::from(y >= 2000), y % 100),
            format!("{h:02}{mi:02}{sec:02}"),
        );
        m.usec = format!("{us:06}");
    }
    // Messages on call message queues are in the job log too.
    if let Ok(mut g) = JOBLOG.lock() {
        g.get_or_insert_with(HashMap::new)
            .entry(job.to_string())
            .or_default()
            .push(m.clone());
    }
    with_stack(job, |s| {
        let mut i = s.iter().position(|e| e.id == id)?;
        // QCMDEXC keeps no messages: those sent to it go to its caller
        // (a command processing program's completion message reaches
        // the program that ran the command).
        while i > 0 && s[i].lib == "QSYS" && s[i].pgm == "QCMDEXC" {
            i -= 1;
        }
        s[i].queue.push(m);
        Some(key)
    })
}

/// Receive a message from an entry's queue (QMHRCVPM message type, key
/// and action *OLD, *REMOVE or *SAME), with the entry it is on.
pub fn stack_receive(
    job: &str,
    id: u64,
    msgtype: &str,
    key: Option<u32>,
    action: &str,
) -> Option<QMsg> {
    with_stack(job, |s| {
        let e = s.iter_mut().find(|e| e.id == id)?;
        let i = select(&e.queue, msgtype, key)?;
        match action {
            "*REMOVE" => Some(e.queue.remove(i)),
            "*SAME" => Some(e.queue[i].clone()),
            _ => {
                e.queue[i].old = true;
                Some(e.queue[i].clone())
            }
        }
    })
}

/// The entry with this id.
pub fn stack_entry(job: &str, id: u64) -> Option<Entry> {
    with_stack(job, |s| s.iter().find(|e| e.id == id).cloned())
}

/// An escape message as an error: its text is "ID: text"; the message
/// itself (data, sender) goes with it to the procedure that handles it.
#[derive(Debug, Clone)]
pub struct Escape(pub QMsg, pub Option<u64>);

impl Escape {
    /// Whether the procedure of call stack entry `current` lets the
    /// escape message pass: it was sent to an entry nearer the start of
    /// the stack (still there), so the entries above it end without
    /// handling it (Send Program Message API, *ESCAPE).
    pub fn passes(&self, job: &str, current: Option<u64>) -> bool {
        match (self.1, current) {
            (Some(t), Some(c)) if t != c => with_stack(job, |s| s.iter().any(|e| e.id == t)),
            _ => false,
        }
    }
}

/// The running procedure's call stack entry.
pub fn stack_top_id(job: &str) -> Option<u64> {
    with_stack(job, |s| s.last().map(|e| e.id))
}

/// Who sent a message: program, module, procedure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sender {
    pub pgm: String,
    pub module: String,
    pub proc: String,
    /// The source line of the statement that sent it (0: not known).
    pub line: usize,
}

impl std::fmt::Display for Escape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = if self.0.msgid.is_empty() {
            "CPF9898"
        } else {
            &self.0.msgid
        };
        write!(f, "{id}: {}", self.0.text)
    }
}

impl std::error::Error for Escape {}

/// The job ended: its named queues go.
pub fn named_end_job(job: &str) {
    crate::ovr::end_job(job);
    if let Ok(mut g) = JOBLOG.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(job);
        }
    }
    if let Ok(mut g) = STACKS.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(job);
        }
    }
    if let Ok(mut g) = NAMED.lock() {
        if let Some(m) = g.as_mut() {
            m.retain(|(j, _), _| j != job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ty: &str, old: bool) -> QMsg {
        QMsg {
            ty: ty.into(),
            old,
            ..Default::default()
        }
    }

    // RCVMSG MSGTYPE (CL reference): *ANY the first new message, *EXCP
    // the last exception, *FIRST/*LAST new or old; *NEXT/*PRV from a key.
    #[test]
    fn selection() {
        let mut q = vec![
            m("*INFO", true),
            m("*COMP", false),
            m("*ESCAPE", false),
            m("*DIAG", false),
            m("*ESCAPE", false),
        ];
        for (i, x) in q.iter_mut().enumerate() {
            x.key = i as u32 + 10;
        }
        assert_eq!(select(&q, "*ANY", None), Some(1));
        assert_eq!(select(&q, "*EXCP", None), Some(4));
        assert_eq!(select(&q, "*DIAG", None), Some(3));
        assert_eq!(select(&q, "*INFO", None), None);
        assert_eq!(select(&q, "*FIRST", None), Some(0));
        assert_eq!(select(&q, "*LAST", None), Some(4));
        assert_eq!(select(&q, "*NEXT", Some(12)), Some(3));
        assert_eq!(select(&q, "*PRV", Some(12)), Some(1));
        assert_eq!(select(&q, "*ANY", Some(10)), Some(0));
        assert_eq!(select(&q, "*ANY", Some(99)), None);
        assert_eq!(parse_key(&key_text(1234)), Some(1234));
        assert_eq!(rtntype("*ESCAPE"), "15");
    }
}
