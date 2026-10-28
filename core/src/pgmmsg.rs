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
    /// The call stack entry it was sent to (its program, library,
    /// module and procedure), for the job log.
    pub receiver: Sender,
    /// When it was sent: CYYMMDD and HHMMSS.
    pub sent: (String, String),
    /// And the microseconds (6 digits).
    pub usec: String,
    /// The sender the job log shows when it is not `sender` (an RPG
    /// run-time error: the run time's QRNXIE, while the message APIs give
    /// programs the procedure where it happened).
    pub logged_sender: Option<Sender>,
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
    /// What commands wrote to the job's terminal (QSH's standard
    /// output): `system` writes it as it comes, before the command's
    /// messages (PUB400 2026-10-23).
    pub terminal: Vec<String>,
    /// The message data of the last command's completion message, when
    /// it has some (CRTSRVPGM: the service program and its library).
    pub message_data: Option<Vec<u8>>,
    /// Messages a CL program received on its own queue (the completion
    /// messages of the commands it ran, what the programs it called sent
    /// it): in what the program returns, for the job log, but `system`
    /// does not show them (PUB400 2026-10-26, tools/golden/verify1.txt
    /// v_call, qsh.txt q_call).
    pub own_queue: Vec<String>,
    /// The CL programs running (with `stack`): name and source line.
    frames: Vec<Frame>,
    /// The escape message a called program's job log entries ended with
    /// (`log_from_program`), until the next message is put.
    logged_escape: Option<String>,
    /// Every message put on a queue, oldest first: the job log's (one
    /// whose program ended stays, PUB400 2026-10-28,
    /// tools/golden/verify17.txt x17_joblog; removed ones leave: `unlog`).
    log: Vec<QMsg>,
}

/// A CL program on the call stack.
#[derive(Debug, Clone, Default)]
struct Frame {
    lib: String,
    pgm: String,
    /// ILE (CRTBNDCL): its module and procedure have its name.
    ile: bool,
    /// The statement running (source line).
    line: usize,
}

impl CallStack {
    /// A program starts: its queue.
    pub fn enter(&mut self) {
        self.enter_program("", "", false);
    }

    /// A CL program `lib/pgm` starts (`ile`: made by CRTBNDCL).
    pub fn enter_program(&mut self, lib: &str, pgm: &str, ile: bool) {
        self.stack.push(Vec::new());
        self.frames.push(Frame {
            lib: crate::qtemp::logical(lib),
            pgm: pgm.to_string(),
            ile,
            line: 0,
        });
    }

    /// A program ends: its queue and the overrides it made go with it.
    pub fn leave(&mut self) {
        let level = self.stack.len();
        self.overrides
            .retain(|o| o.level < level || o.actgrp.is_some());
        self.stack.pop();
        self.frames.pop();
    }

    /// The running program is at a statement (its source line).
    pub fn set_line(&mut self, line: usize) {
        if let Some(f) = self.frames.last_mut() {
            f.line = line;
        }
    }

    /// The running CL program as a message's sender (or receiver).
    pub fn current(&self) -> Option<Sender> {
        self.frames.last().map(frame_end)
    }

    /// Messages a program sent with the message APIs (a called RPG
    /// program's), into this job's log. An escape message among them is
    /// the one the caller gets: not logged a second time when the caller
    /// takes it (h21_joblog: GLD21Q's CPF9898 once, to its entry
    /// procedure, though GLD21A's MONMSG took it).
    pub fn log_from_program(&mut self, msgs: Vec<QMsg>) {
        self.logged_escape = msgs
            .iter()
            .rev()
            .find(|m| m.ty == "*ESCAPE")
            .map(|m| m.msgid.clone());
        self.log.extend(msgs);
    }

    /// The messages of the job log that are still there, oldest first.
    pub fn logged(&self) -> &[QMsg] {
        &self.log
    }

    /// Messages removed from their queue leave the job log, whatever
    /// their type (PUB400 2026-10-28: impromptu messages a program removed,
    /// verify12 n12_keys and verify13 t13_joblog; a called program's
    /// escape taken with RCVMSG RMV(*YES), verify17 x17_joblog; completion
    /// messages, its own and CRTDTAARA's, taken with RCVMSG RMV(*YES) or
    /// RMVMSG, verify18 y18_joblog).
    fn unlog(&mut self, keys: &[u32]) {
        self.log.retain(|m| !keys.contains(&m.key));
    }
}

/// A CL program as a sender or receiver; its statement number is its
/// source line times 100 (PUB400 2026-10-28, tools/golden/verify17.txt
/// x17_joblog: the SNDPGMMSG on line 7 is statement 700).
fn frame_end(f: &Frame) -> Sender {
    Sender {
        pgm: f.pgm.clone(),
        lib: f.lib.clone(),
        module: if f.ile { f.pgm.clone() } else { String::new() },
        proc: if f.ile { f.pgm.clone() } else { String::new() },
        line: f.line * 100,
        inst: String::new(),
    }
}

/// The program below a job's first CL program, which gets what that one
/// sends to its caller: QCMDEXC (QSYS, instruction 012F), the program
/// `system` runs the command with (PUB400 2026-10-28,
/// tools/golden/verify17.txt x17_joblog and verify13.txt t13_joblog).
pub fn command_caller() -> Sender {
    Sender {
        pgm: "QCMDEXC".into(),
        lib: "QSYS".into(),
        inst: "012F".into(),
        ..Default::default()
    }
}

/// The program that sends a command's message (its command processing
/// program, or the one it calls), with the instruction, as PUB400's job
/// logs show them (2026-10-28, all in QSYS): CHKOBJ's escape CPF9801
/// from QLICKOBJ 025B (verify17 x17_joblog); CRTDTAARA's CPC0904 from
/// QWCCCRVC 0095, DLTDTAARA's and DLTF's messages from QLIDLOBJ 0772
/// (verify18 y18_joblog, verify19 b19_joblog); CRTPF's CPC7301 from QDDCPF
/// 054C and its CPC7305 from QDDCPFM 005B, CLRPFM's CPC3101 from
/// QDBCLRPF 03AE, ADDLIBLE's CPC2196 from QLICUSRL 0114 (b19_joblog).
/// Others are not known here: `*N` (docs/fidelity-gaps.md).
pub fn command_program(command: &str, msgid: &str) -> Sender {
    let (pgm, lib, inst) = match (command.to_ascii_uppercase().as_str(), msgid) {
        ("CHKOBJ", _) => ("QLICKOBJ", "QSYS", "025B"),
        ("CRTDTAARA", _) => ("QWCCCRVC", "QSYS", "0095"),
        ("DLTDTAARA" | "DLTF", _) => ("QLIDLOBJ", "QSYS", "0772"),
        ("CRTPF", "CPC7301") => ("QDDCPF", "QSYS", "054C"),
        ("CRTPF", "CPC7305") => ("QDDCPFM", "QSYS", "005B"),
        ("CLRPFM", "CPC3101") => ("QDBCLRPF", "QSYS", "03AE"),
        ("ADDLIBLE", "CPC2196") => ("QLICUSRL", "QSYS", "0114"),
        _ => ("*N", "", "*N"),
    };
    Sender {
        pgm: pgm.into(),
        lib: lib.into(),
        inst: inst.into(),
        ..Default::default()
    }
}

/// The severity of a system message whose description is not here: as
/// captured (PUB400 2026-10-28, tools/golden/verify17.txt x17_joblog:
/// CPF9898 and CPF9801 40, CPF1124 00; verify18 y18_joblog: CPF2105 40,
/// CPC0904 00; verify21 h21_joblog: RNX0100 50); others 00
/// (docs/fidelity-gaps.md).
pub fn system_severity(id: &str) -> i32 {
    match id {
        "CPF9898" | "CPF9801" | "CPF2105" => 40,
        // An RPG run-time error (h21_joblog: RNX0100).
        "RNX0100" => 50,
        _ => 0,
    }
}

/// Who sends an RPG run-time error's escape message: the RPG run time's
/// QRNXIE in QSYS, module QRNXMSG, procedure SignalException, statement
/// 22, as PUB400's job log showed it (2026-10-28,
/// tools/golden/verify21.txt h21_joblog: RNX0100 after %SUBST).
pub fn rpg_runtime_sender() -> Sender {
    Sender {
        pgm: "QRNXIE".into(),
        lib: "QSYS".into(),
        module: "QRNXMSG".into(),
        proc: "SignalException".into(),
        line: 22,
        inst: "*STMT".into(),
    }
}

/// When a message was sent: CYYMMDD, HHMMSS and microseconds, now (if
/// not set yet).
pub fn stamp(m: &mut QMsg) {
    if m.sent.0.is_empty() {
        let ((y, mo, d), (h, mi, sec), us) = crate::rpg::dates::now();
        m.sent = (
            format!("{}{:02}{mo:02}{d:02}", u8::from(y >= 2000), y % 100),
            format!("{h:02}{mi:02}{sec:02}"),
        );
        m.usec = format!("{us:06}");
    }
}

/// When the job started (the process, for `system`): the first time
/// this is asked.
pub fn job_started() -> crate::rpg::dates::Ts {
    static STARTED: std::sync::OnceLock<crate::rpg::dates::Ts> = std::sync::OnceLock::new();
    *STARTED.get_or_init(crate::rpg::dates::now)
}

impl CallStack {
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
        stamp(&mut m);
        if !m.msgid.is_empty() && !m.text.is_empty() {
            self.texts.insert(m.msgid.clone(), m.text.clone());
        }
        let n = self.stack.len();
        let at = if caller {
            n.checked_sub(2)
        } else {
            n.checked_sub(1)
        };
        // Sent by the running program unless the sender is known (a
        // called program's escape, a command's message); received by the
        // program whose queue it goes on (or the command's caller).
        if m.sender.pgm.is_empty() {
            if let Some(s) = self.current() {
                m.sender = s;
            }
        }
        let logged = m.ty == "*ESCAPE" && self.logged_escape.take().is_some_and(|id| id == m.msgid);
        self.logged_escape = None;
        if n > 0 && !logged {
            m.receiver = at
                .and_then(|i| self.frames.get(i))
                .map(frame_end)
                .unwrap_or_else(command_caller);
            self.log.push(m.clone());
        }
        if let Some(q) = at.and_then(|i| self.stack.get_mut(i)) {
            q.push(m);
        }
        k
    }

    /// The messages on the running program's queue (to tell those a
    /// command put there from those it only returned).
    pub fn current_queue(&self) -> &[QMsg] {
        self.stack.last().map(|q| q.as_slice()).unwrap_or(&[])
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
        let mut n = self.stack.len().checked_sub(1 + up)?;
        // A message key names one message in the job: RCVMSG finds it
        // whichever call stack entry's queue it is on (PUB400 2026-10-28,
        // tools/golden/verify11.txt k_rows: a key from the caller found
        // with PGMQ(*SAME) and with PGMQ(*PRV *)).
        if let Some(k) = key {
            if !self.stack[n].iter().any(|m| m.key == k) {
                n = (0..self.stack.len())
                    .rev()
                    .find(|&i| self.stack[i].iter().any(|m| m.key == k))
                    .unwrap_or(n);
            }
        }
        let q = self.stack.get_mut(n)?;
        let i = select(q, msgtype, key)?;
        let exception = matches!(q[i].ty.as_str(), "*ESCAPE" | "*NOTIFY");
        let remove = match rmv {
            "*NO" => false,
            "*KEEPEXCP" => !exception,
            _ => true,
        };
        if remove {
            let m = q.remove(i);
            self.unlog(&[m.key]);
            Some(m)
        } else {
            q[i].old = true;
            Some(q[i].clone())
        }
    }
}

impl CallStack {
    /// RMVMSG on the queue `up` levels above the running program's:
    /// CLEAR(*ALL), *OLD, *NEW, *KEEPUNANS (here: all, as there are no
    /// inquiry messages), or the message with `key` (CLEAR(*BYKEY)).
    pub fn remove_from(&mut self, up: usize, clear: &str, key: Option<u32>) {
        let Some(n) = self.stack.len().checked_sub(1 + up) else {
            return;
        };
        let Some(q) = self.stack.get_mut(n) else {
            return;
        };
        let gone: Vec<u32> = match clear {
            "*OLD" | "*NEW" | "*ALL" | "*KEEPUNANS" => {
                let (gone, keep): (Vec<QMsg>, Vec<QMsg>) =
                    std::mem::take(q).into_iter().partition(|m| match clear {
                        "*OLD" => m.old,
                        "*NEW" => !m.old,
                        _ => true,
                    });
                *q = keep;
                gone.iter().map(|m| m.key).collect()
            }
            _ => match key {
                // By key: the message wherever it is on the call stack,
                // as RCVMSG finds it (PUB400 2026-10-28,
                // tools/golden/verify12.txt n12_rows: GLD12B removed its
                // caller's message by key, and the caller's own RCVMSG of
                // that key then failed with CPF2410).
                Some(k) => {
                    if q.iter().any(|m| m.key == k) {
                        q.retain(|m| m.key != k);
                    } else {
                        for q in self.stack.iter_mut() {
                            q.retain(|m| m.key != k);
                        }
                    }
                    vec![k]
                }
                None => vec![],
            },
        };
        self.unlog(&gone);
    }

    /// Whether a program message queue on the call stack has the message
    /// with this key.
    pub fn has_key(&self, k: u32) -> bool {
        self.stack.iter().any(|q| q.iter().any(|m| m.key == k))
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

/// A message key as the message APIs give it to a program (QMHSNDPM,
/// QMHRCVPM, QGYOLJBL): four bytes, binary, as IBM i's are.
pub fn key_bytes(k: u32) -> [u8; 4] {
    k.to_be_bytes()
}

/// A message key a program passes to the message APIs; blanks (or
/// fewer than four bytes) are no key.
pub fn key_from_bytes(b: &[u8]) -> Option<u32> {
    let b: [u8; 4] = b.get(..4)?.try_into().ok()?;
    if b == [0x40; 4] {
        return None;
    }
    Some(u32::from_be_bytes(b))
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
    // An RPG escape message (RNX) of an error written as its inquiry
    // message (RNQ) of the same number.
    let alt = id
        .strip_prefix("RNX")
        .map(|n| format!("RNQ{n}"))
        .unwrap_or_else(|| id.to_string());
    let line = s
        .lines()
        .find(|l| l.starts_with(id) || l.starts_with(&alt))
        .or_else(|| s.lines().next())
        .unwrap_or("");
    let text = line
        .strip_prefix(id)
        .or_else(|| line.strip_prefix(alt.as_str()))
        .map(|t| t.trim_start_matches(':').trim())
        .unwrap_or(line)
        .to_string();
    QMsg {
        msgid: id.to_string(),
        msgf: crate::msgf::system_msgf(id).into(),
        msgflib: "QSYS".into(),
        sndmsgflib: "*LIBL".into(),
        text,
        sev: system_severity(id),
        ty: "*ESCAPE".into(),
        ..Default::default()
    }
}

/// The message files of the system: they have no descriptions here.
fn is_system_msgf(name: &str) -> bool {
    matches!(
        name,
        "QCPFMSG" | "QRNXMSG" | "QSQLMSG" | "QCPFMSGL" | "QCPFMSGU" | "QZSHMSGF"
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
        vec![crate::qtemp::real(&lib)]
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
            m.sev = system_severity(&msgid);
        }
        None => bail!("CPF2407: There is no message file {name} in library {lib}."),
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
    /// A program's main procedure, with the program entry procedure
    /// below it ([`Target`]).
    pub pep: bool,
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

/// What a job's programs wrote to standard output (C printf, puts): its
/// lines, and whether the last one is still open (no newline yet).
type Stdout = HashMap<String, (Vec<String>, bool)>;
static STDOUT: std::sync::Mutex<Option<Stdout>> = std::sync::Mutex::new(None);

/// A program writes to the job's standard output.
pub fn write_stdout(job: &str, text: &str) {
    let mut g = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    let (lines, open) = g
        .get_or_insert_with(HashMap::new)
        .entry(job.to_string())
        .or_default();
    for (i, part) in text.split('\n').enumerate() {
        if i > 0 {
            *open = false;
        }
        if part.is_empty() && i > 0 {
            continue;
        }
        match lines.last_mut() {
            Some(last) if *open => last.push_str(part),
            _ => lines.push(part.to_string()),
        }
        *open = true;
    }
    if text.ends_with('\n') {
        *open = false;
    }
}

/// The job's standard output so far, taken.
pub fn take_stdout(job: &str) -> Vec<String> {
    let mut g = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    g.as_mut()
        .and_then(|m| m.remove(job))
        .map(|(l, _)| l)
        .unwrap_or_default()
}
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
            pep: false,
        })
    });
    id
}

/// Call stack entries kept for listing only (QWVRCSTK): CL programs and
/// the command analyzer under a validity checker. Their messages go
/// through the CL call stack (`Job.calls`), so the message functions
/// above do not see them.
static LISTED: std::sync::Mutex<Option<HashMap<String, Vec<Entry>>>> = std::sync::Mutex::new(None);

/// A CL program (or QCATRS) starts: an entry for listings.
pub fn listed_push(job: &str, (lib, pgm): (&str, &str), actgrp: &str) -> u64 {
    let id = ENTRY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut g = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .entry(job.to_string())
        .or_default()
        .push(Entry {
            id,
            lib: lib.to_string(),
            line: 0,
            pgm: pgm.to_string(),
            module: if lib == "QSYS" {
                String::new()
            } else {
                pgm.to_string()
            },
            proc: if lib == "QSYS" {
                String::new()
            } else {
                pgm.to_string()
            },
            actgrp: actgrp.to_string(),
            queue: Vec::new(),
            pep: false,
        });
    id
}

/// It ended.
pub fn listed_pop(job: &str, id: u64) {
    let mut g = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut().and_then(|m| m.get_mut(job)) {
        if let Some(i) = s.iter().position(|e| e.id == id) {
            s.truncate(i);
        }
    }
}

/// A listed entry's procedure name (an ILE program's entry procedure).
pub fn listed_set_proc(job: &str, id: u64, proc: &str) {
    let mut g = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = g
        .as_mut()
        .and_then(|m| m.get_mut(job))
        .and_then(|s| s.iter_mut().find(|e| e.id == id))
    {
        e.proc = proc.to_string();
    }
}

/// Every entry, innermost last: the procedures' and the listed ones in
/// the order they started.
pub fn stack_listed(job: &str) -> Vec<Entry> {
    let mut all = stack(job);
    if let Some(l) = LISTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(job))
    {
        all.extend(l.iter().cloned());
    }
    all.sort_by_key(|e| e.id);
    all
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

/// Where a message API's message goes, counting the program entry
/// procedure each RPG program has below its main procedure, as IBM i
/// does (PUB400 2026-10-28, tools/golden/verify19.txt b19_joblog and
/// verify20.txt e20_joblog: '*' counter 1 from a program's main
/// procedure goes to `_QRNP_PEP_` and its name, also when an RPG program
/// called it; *PGMBDY is that entry procedure, so *PGMBDY counter 1 is
/// the caller's procedure).
#[derive(Debug, Clone)]
pub enum Target {
    Entry(u64),
    /// The entry procedure of the program whose main procedure this is.
    Pep(Entry),
}

/// [`Target`] of an entry and counter (as [`stack_target`] names
/// them); None: above the outermost RPG entry.
pub fn stack_target_pep(job: &str, entry: &str, counter: usize) -> Option<Target> {
    with_stack(job, |s| {
        // The entries with the entry procedures, outermost first.
        let mut v: Vec<Target> = Vec::new();
        let mut at = Vec::new();
        for e in s.iter() {
            if e.pep {
                v.push(Target::Pep(e.clone()));
            }
            at.push(v.len());
            v.push(Target::Entry(e.id));
        }
        let top = s.len().checked_sub(1)?;
        let e = entry.trim().to_ascii_uppercase();
        let base = match e.as_str() {
            "*" | "" => at[top],
            "*PGMBDY" => {
                let pgm = &s[top].pgm;
                let mut i = top;
                while i > 0 && s[i - 1].pgm == *pgm && !s[i].pep {
                    i -= 1;
                }
                if s[i].pep {
                    at[i] - 1
                } else {
                    at[i]
                }
            }
            "*CTLBDY" => {
                let g = &s[top].actgrp;
                let mut i = top;
                while i > 0 && s[i - 1].actgrp == *g {
                    i -= 1;
                }
                // The oldest entry of the activation group: the entry
                // procedure of its first program, which is in it too.
                if s[i].pep {
                    at[i] - 1
                } else {
                    at[i]
                }
            }
            name => {
                at[s.iter().rposition(|x| {
                    x.proc.eq_ignore_ascii_case(name) || x.pgm.eq_ignore_ascii_case(name)
                })?]
            }
        };
        base.checked_sub(counter).map(|i| v[i].clone())
    })
}

/// An RPG program's main procedure: its program entry procedure is
/// below it (see [`Target`]).
pub fn stack_mark_pep(job: &str, id: u64) {
    with_stack(job, |s| {
        if let Some(e) = s.iter_mut().find(|e| e.id == id) {
            e.pep = true;
        }
    })
}

/// A message sent to the program entry procedure of `main`'s program
/// (`_QRNP_PEP_` and the program's name, an ILE RPG program's, with no
/// statement: PUB400 2026-10-28, tools/golden/verify19.txt b19_joblog):
/// it is only in the job log; its key.
pub fn pep_send(job: &str, main: &Entry, mut m: QMsg) -> u32 {
    let key = NAMED_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    m.key = key;
    stamp(&mut m);
    m.receiver = Sender {
        pgm: main.pgm.clone(),
        lib: crate::qtemp::logical(&main.lib),
        module: main.module.clone(),
        proc: format!("_QRNP_PEP_{}", main.pgm),
        line: 0,
        inst: String::new(),
    };
    if let Ok(mut g) = JOBLOG.lock() {
        g.get_or_insert_with(HashMap::new)
            .entry(job.to_string())
            .or_default()
            .push(m);
    }
    key
}

/// Put a message on an entry's queue; its key.
pub fn stack_send(job: &str, id: u64, m: QMsg) -> Option<u32> {
    stack_send_logged(job, id, m, true)
}

/// Put a message that is in the job log already (an escape message the
/// job log shows where it was sent, PUB400 2026-10-28,
/// tools/golden/verify21.txt h21_joblog) on an entry's queue.
pub fn stack_send_unlogged(job: &str, id: u64, m: QMsg) -> Option<u32> {
    stack_send_logged(job, id, m, false)
}

fn stack_send_logged(job: &str, id: u64, mut m: QMsg, log: bool) -> Option<u32> {
    let key = NAMED_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    m.key = key;
    stamp(&mut m);
    // Messages on call message queues are in the job log too.
    if !log {
    } else if let Ok(mut g) = JOBLOG.lock() {
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
        m.receiver = Sender {
            pgm: s[i].pgm.clone(),
            lib: crate::qtemp::logical(&s[i].lib),
            module: s[i].module.clone(),
            proc: s[i].proc.clone(),
            // The running procedure is at the statement that sends
            // (b19_joblog: GLD19P to itself, statement 21 both).
            line: if m.receiver.line > 0 {
                m.receiver.line
            } else if i + 1 == s.len() && m.sender.line > 0 {
                m.sender.line
            } else {
                s[i].line
            },
            inst: String::new(),
        };
        // The job log's copy names the receiver too.
        if let Ok(mut g) = JOBLOG.lock() {
            if let Some(l) = g.as_mut().and_then(|j| j.get_mut(job)) {
                if let Some(x) = l.iter_mut().rev().find(|x| x.key == key) {
                    x.receiver = m.receiver.clone();
                }
            }
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
    let r = with_stack(job, |s| {
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
    });
    if action == "*REMOVE" {
        if let Some(m) = &r {
            forget_logged(job, &[m.key]);
        }
    }
    r
}

/// Receive the message with `key` from whichever call stack entry's
/// queue has it (message keys are the job's): the message and that
/// entry's id.
pub fn stack_receive_key(
    job: &str,
    msgtype: &str,
    key: u32,
    action: &str,
) -> Option<(QMsg, Option<u64>)> {
    let id = with_stack(job, |s| {
        s.iter()
            .rev()
            .find(|e| select(&e.queue, msgtype, Some(key)).is_some())
            .map(|e| e.id)
    });
    if let Some(id) = id {
        return stack_receive(job, id, msgtype, Some(key), action).map(|m| (m, Some(id)));
    }
    // A message on the queue of a call stack entry that has ended is
    // still the job's (its job log keeps it): received from there
    // (iRPGUnit's getLatestEscMsg receives the escape message a
    // procedure that has returned took).
    let mut g = JOBLOG.lock().ok()?;
    let l = g.as_mut()?.get_mut(job)?;
    let i = select(l, msgtype, Some(key))?;
    let m = match action {
        "*REMOVE" => l.remove(i),
        "*SAME" => l[i].clone(),
        _ => {
            l[i].old = true;
            l[i].clone()
        }
    };
    Some((m, None))
}

/// QMHRMVPM on a call stack entry's queue: *ALL, *BYKEY (the message
/// with `key`), *OLD, *NEW, *KEEPRQS (all but requests).
pub fn stack_remove(job: &str, id: u64, which: &str, key: Option<u32>) {
    let gone = with_stack(job, |s| {
        let e = s.iter_mut().find(|e| e.id == id)?;
        let keep = |m: &QMsg| match which {
            "*BYKEY" => Some(m.key) != key,
            "*OLD" => !m.old,
            "*NEW" => m.old,
            "*KEEPRQS" => m.ty == "*RQS",
            _ => false,
        };
        let gone: Vec<u32> = e.queue.iter().filter(|m| !keep(m)).map(|m| m.key).collect();
        e.queue.retain(keep);
        Some(gone)
    })
    .unwrap_or_default();
    forget_logged(job, &gone);
}

/// Messages removed from their queue leave the job log too (IBM i: a
/// program message queue's messages are the job log's).
fn forget_logged(job: &str, keys: &[u32]) {
    if keys.is_empty() {
        return;
    }
    if let Ok(mut g) = JOBLOG.lock() {
        if let Some(l) = g.as_mut().and_then(|m| m.get_mut(job)) {
            l.retain(|m| !keys.contains(&m.key));
        }
    }
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
    /// The library of the program or service program (as the job sees
    /// it: QTEMP, not the job's own library).
    pub lib: String,
    pub module: String,
    pub proc: String,
    /// The statement that sent it (0: not known): an RPG program's
    /// source line, a CL program's line times 100 (as job logs number
    /// them: PUB400 2026-10-28, tools/golden/verify19.txt b19_joblog).
    pub line: usize,
    /// The instruction a system program sent it from (the job log's INST
    /// column); blank: `*STMT` for an ILE procedure, else not known.
    pub inst: String,
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

    // PUB400 2026-10-28, tools/golden/verify11.txt k_rows: GLD11A sends
    // 'GLD11 key test' to its own queue and passes the key to GLD11B,
    // whose RCVMSG PGMQ(*SAME) MSGKEY(&K) RMV(*NO) and RCVMSG
    // PGMQ(*PRV *) MSGKEY(&K) RMV(*NO) both receive it (K01, K02).
    #[test]
    fn key_from_another_call_stack_entry() {
        let mut c = CallStack::default();
        c.enter();
        let k = c.put(
            QMsg {
                text: "GLD11 key test".into(),
                ty: "*INFO".into(),
                ..Default::default()
            },
            false,
        );
        c.enter();
        let same = c.receive_from(0, "*ANY", Some(k), "*NO").unwrap();
        assert_eq!(same.text, "GLD11 key test");
        let prv = c.receive_from(1, "*ANY", Some(k), "*NO").unwrap();
        assert_eq!(prv.text, "GLD11 key test");
        // A key no queue has: nothing.
        assert!(c.receive_from(0, "*ANY", Some(k + 99), "*NO").is_none());
        // verify12 n12_rows: RMVMSG PGMQ(*SAME) MSGKEY in the called
        // program removes the caller's message; the key is then gone.
        c.remove_from(0, "*BYKEY", Some(k));
        assert!(!c.has_key(k));
    }

    // An RPG program GLD20P calls GLD20Q (PUB400 2026-10-28,
    // tools/golden/verify20.txt e20_joblog): from GLD20Q's main
    // procedure '*' counter 1 is GLD20Q's entry procedure, *PGMBDY
    // counter 1 is GLD20P's main procedure. *CTLBDY is the entry procedure
    // of the activation group's first program, so counter 1 is above it
    // (none here): RPGUnit's sndCompMsg sends its completion message so,
    // and its self-test passes on IBM i.
    #[test]
    fn entry_procedures() {
        let job = "TEST-PEP";
        let p = stack_push(job, ("LIB", "GLD20P"), "GLD20P", "GLD20P", "QILE");
        stack_mark_pep(job, p);
        let q = stack_push(job, ("LIB", "GLD20Q"), "GLD20Q", "GLD20Q", "QILE");
        stack_mark_pep(job, q);
        let pep_of = |t: Option<Target>| match t {
            Some(Target::Pep(e)) => Some(e.pgm),
            _ => None,
        };
        let entry = |t: Option<Target>| match t {
            Some(Target::Entry(id)) => Some(id),
            _ => None,
        };
        assert_eq!(
            pep_of(stack_target_pep(job, "*", 1)).as_deref(),
            Some("GLD20Q")
        );
        assert_eq!(entry(stack_target_pep(job, "*", 0)), Some(q));
        assert_eq!(entry(stack_target_pep(job, "*PGMBDY", 1)), Some(p));
        assert_eq!(
            pep_of(stack_target_pep(job, "*", 3)).as_deref(),
            Some("GLD20P")
        );
        assert_eq!(
            pep_of(stack_target_pep(job, "*CTLBDY", 0)).as_deref(),
            Some("GLD20P")
        );
        assert!(stack_target_pep(job, "*CTLBDY", 1).is_none());
        stack_pop(job, p);
    }
}
