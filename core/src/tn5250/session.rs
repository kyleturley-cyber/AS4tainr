//! One interactive 5250 session: sign-on, password change, command entry,
//! and list displays. The screen texts are ibmish's own; the layout follows
//! common 5250 conventions (title on line 1, function keys on line 22 or
//! 23, messages on line 24).

use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::ds::{self, attr, Aid, FieldKind, Screen};
use super::telnet::{opcode, Connection};
use crate::bridge;
use crate::commands::{Environment, Job, ListDisplay, Outcome};
use crate::dspf;
use crate::users;
use crate::workstn;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub env: Environment,
    pub system_name: String,
    /// Libraries added to every job's library list after the current library.
    pub libl: Vec<String>,
    /// Sign-on attempts per connection before it is closed.
    pub max_signon_attempts: u32,
}

static DEVICE_SEQ: AtomicU32 = AtomicU32::new(1);

/// Serve 5250 sessions on a listener until the task is dropped.
pub async fn serve(listener: tokio::net::TcpListener, cfg: ServerConfig) -> Result<()> {
    let cfg = Arc::new(cfg);
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg = cfg.clone();
        let device = format!("QPADEV{:04}", DEVICE_SEQ.fetch_add(1, Ordering::SeqCst));
        tokio::spawn(async move {
            match run(stream, &cfg, &device).await {
                Ok(()) => eprintln!("ibmish: {device} ({peer}) session ended"),
                Err(e) => eprintln!("ibmish: {device} ({peer}) session ended: {e:#}"),
            }
        });
    }
}

struct Term {
    conn: Connection,
}

impl Term {
    async fn show(&mut self, s: &Screen) -> Result<ds::Input> {
        self.conn
            .send_record(opcode::PUT_GET, &s.to_stream())
            .await?;
        let data = self.conn.read_record().await?;
        Ok(ds::parse_input(&data, s))
    }
}

fn centered(s: &mut Screen, row: u8, text: &str) {
    let col = ((80 - text.chars().count() as u8) / 2).max(2);
    s.text(row, col, attr::HIGH, text);
}

fn keys(s: &mut Screen, row: u8, text: &str) {
    s.text(row, 2, attr::BLUE, text);
}

/// Labels padded with dots, in the usual 5250 prompt style.
fn label(text: &str, width: usize) -> String {
    let mut s = text.to_string();
    if s.len().is_multiple_of(2) {
        s.push(' ');
    }
    while s.len() + 2 <= width {
        s.push_str(" .");
    }
    s
}

pub async fn run(stream: tokio::net::TcpStream, cfg: &ServerConfig, device: &str) -> Result<()> {
    let mut conn = Connection::new(stream);
    conn.negotiate().await?;
    let mut term = Term { conn };
    let client = bridge::connect_db(&cfg.env.database_url).await?;
    let mut message = String::new();
    let mut attempts = 0;

    loop {
        // ---- Sign on --------------------------------------------------
        let mut s = Screen::new();
        centered(&mut s, 1, "Sign On");
        s.put(
            2,
            48,
            &format!("{}:   {}", label("System", 18), cfg.system_name),
        );
        s.put(3, 48, &format!("{}:   QINTER", label("Subsystem", 18)));
        s.put(4, 48, &format!("{}:   {device}", label("Display", 18)));
        s.put(6, 17, &label("User", 34));
        s.field("USER", 6, 53, 10, FieldKind::Upper, "");
        s.put(7, 17, &label("Password", 34));
        s.field(
            "PASSWORD",
            7,
            53,
            users::MAX_PASSWORD as u16,
            FieldKind::Hidden,
            "",
        );
        s.put(8, 17, &label("Current library", 34));
        s.field("CURLIB", 8, 53, 10, FieldKind::Upper, "");
        s.text(
            22,
            15,
            attr::BLUE,
            "ibmish - an ibm-ish experience for open source devs",
        );
        s.message(&message);
        let inp = term.show(&s).await?;
        message.clear();
        if inp.aid != Aid::Enter {
            continue;
        }
        let user = inp.get("USER").trim().to_string();
        if user.is_empty() {
            message = "Type your user profile name and password.".into();
            continue;
        }
        let profile = match users::authenticate(&client, &user, inp.get("PASSWORD")).await {
            Ok(p) => p,
            Err(e) => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(800)).await;
                if attempts >= cfg.max_signon_attempts {
                    let mut s = Screen::new();
                    centered(&mut s, 1, "Sign On");
                    s.message("Too many sign-on attempts. The session is ended.");
                    term.conn
                        .send_record(opcode::PUT_GET, &s.to_stream())
                        .await?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    return Ok(());
                }
                message = e.to_string();
                continue;
            }
        };
        attempts = 0;

        if profile.pwd_expired {
            match change_password(&mut term, &client, &profile.name, true).await? {
                true => {
                    message = "Password changed successfully. Sign on with the new password.".into()
                }
                false => message = "Password not changed. Sign on again.".into(),
            }
            continue;
        }

        let curlib = match inp.get("CURLIB").trim() {
            "" => profile.curlib.clone(),
            c => c.to_string(),
        };
        // Programs called from this session show their display files here.
        let (display_tx, mut display_rx) = tokio::sync::mpsc::channel::<bridge::DisplayRequest>(1);
        let mut job = Job {
            client: &client,
            env: &cfg.env,
            user: profile.name.clone(),
            curlib,
            libl: cfg.libl.clone(),
            display: Some(display_tx),
        };
        command_entry(&mut term, &mut job, cfg, &mut display_rx).await?;
        message = format!("User {} signed off.", profile.name);
    }
}

/// Returns true when the password was changed.
async fn change_password(
    term: &mut Term,
    client: &tokio_postgres::Client,
    user: &str,
    expired: bool,
) -> Result<bool> {
    let mut message = if expired {
        "Your password has expired and must be changed.".to_string()
    } else {
        String::new()
    };
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, "Change Password");
        s.put(3, 2, &format!("User profile . . . . . . . . :   {user}"));
        s.put(5, 2, "Type choices, press Enter.");
        s.put(7, 5, &label("Current password", 34));
        s.field(
            "CURRENT",
            7,
            41,
            users::MAX_PASSWORD as u16,
            FieldKind::Hidden,
            "",
        );
        s.put(9, 5, &label("New password", 34));
        s.field(
            "NEW",
            9,
            41,
            users::MAX_PASSWORD as u16,
            FieldKind::Hidden,
            "",
        );
        s.put(10, 5, &label("New password (to verify)", 34));
        s.field(
            "VERIFY",
            10,
            41,
            users::MAX_PASSWORD as u16,
            FieldKind::Hidden,
            "",
        );
        keys(&mut s, 23, "F3=Exit   F12=Cancel");
        s.message(&message);
        let inp = term.show(&s).await?;
        match inp.aid {
            Aid::F(3) | Aid::F(12) => return Ok(false),
            Aid::Enter => {}
            _ => continue,
        }
        if inp.get("NEW") != inp.get("VERIFY") {
            message = "New password and verify password do not match.".into();
            continue;
        }
        match users::change_password(client, user, inp.get("CURRENT"), inp.get("NEW")).await {
            Ok(()) => return Ok(true),
            Err(e) => message = e.to_string(),
        }
    }
}

const LOG_TOP: u8 = 3;
const LOG_ROWS: usize = 16; // rows 3..=18

async fn command_entry(
    term: &mut Term,
    job: &mut Job<'_>,
    cfg: &ServerConfig,
    display_rx: &mut tokio::sync::mpsc::Receiver<bridge::DisplayRequest>,
) -> Result<()> {
    let mut log: Vec<String> = vec![format!("Signed on to {} as {}.", cfg.system_name, job.user)];
    let mut message = String::new();
    let mut retrieve: Vec<String> = Vec::new();
    let mut retrieve_pos = 0usize;
    let mut prefill = String::new();
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, "Command Entry");
        s.put(1, 66, &format!("{:>14}", cfg.system_name));
        s.put(2, 66, &format!("{:>14}", job.user));
        let start = log.len().saturating_sub(LOG_ROWS);
        for (i, line) in log[start..].iter().enumerate() {
            s.put(LOG_TOP + i as u8, 2, line);
        }
        s.put(19, 2, "Type command, press Enter.");
        s.text(20, 2, attr::HIGH, "===>");
        s.field("CMD", 20, 7, 74, FieldKind::Text, &prefill);
        keys(&mut s, 22, "F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel");
        s.message(&message);
        prefill.clear();
        message.clear();
        let inp = term.show(&s).await?;
        match inp.aid {
            Aid::F(3) => return Ok(()),
            Aid::F(12) => continue,
            Aid::F(4) => {
                message =
                    "Prompting (F4) is not available yet. Type the command with its parameters."
                        .into();
                prefill = inp.get("CMD").to_string();
                continue;
            }
            Aid::F(9) => {
                if retrieve.is_empty() {
                    message = "There are no commands to retrieve.".into();
                } else {
                    retrieve_pos = if retrieve_pos == 0 {
                        retrieve.len()
                    } else {
                        retrieve_pos
                    };
                    retrieve_pos -= 1;
                    prefill = retrieve[retrieve_pos].clone();
                }
                continue;
            }
            Aid::Enter => {}
            _ => continue,
        }
        let line = inp.get("CMD").trim().to_string();
        if line.is_empty() {
            continue;
        }
        retrieve.push(line.clone());
        retrieve_pos = 0;
        log.push(format!("> {line}"));
        // Run the command; while it runs, show any display file records
        // its program writes (EXFMT) on this terminal.
        let user = job.user.clone();
        let run = job.run(&line);
        tokio::pin!(run);
        let result = loop {
            tokio::select! {
                r = &mut run => break r,
                Some(req) = display_rx.recv() => {
                    let resp = exfmt(term, &req, &user, &cfg.system_name).await;
                    let failed = resp.is_err();
                    let _ = req.reply.send(resp.map_err(|e| format!("{e:#}")));
                    if failed {
                        anyhow::bail!("the display stopped responding");
                    }
                }
            }
        };
        match result {
            Ok(Outcome::Messages(msgs)) => {
                if let Some(last) = msgs.last() {
                    message = last.clone();
                }
                log.extend(msgs.into_iter().map(|m| format!("  {m}")));
            }
            Ok(Outcome::Display(d)) => {
                list_display(term, &d).await?;
            }
            Ok(Outcome::SignOff) => return Ok(()),
            Err(e) => {
                let text = e.to_string();
                let lines: Vec<&str> = text.lines().collect();
                message = lines.last().copied().unwrap_or("").to_string();
                log.extend(lines.iter().map(|m| format!("  {m}")));
                prefill = line.clone();
            }
        }
    }
}

/// Show an EXFMT frame until the operator enters valid input. Paging
/// through a subfile happens here, without the program.
async fn exfmt(
    term: &mut Term,
    req: &bridge::DisplayRequest,
    user: &str,
    system: &str,
) -> Result<dspf::Response> {
    let (date, time) = clock();
    let ctx = dspf::Context {
        user,
        system,
        date,
        time,
    };
    let mut it = workstn::Interaction::new(&req.frame);
    loop {
        let screen = it.screen(&ctx);
        let inp = term.show(&screen).await?;
        if let workstn::Step::Done(resp) = it.input(&inp) {
            return Ok(resp);
        }
    }
}

/// Job date (MM/DD/YY) and time (HH:MM:SS) in UTC.
fn clock() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (
        format!("{m:02}/{d:02}/{:02}", y % 100),
        format!("{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60),
    )
}

const LIST_ROWS: usize = 12;

async fn list_display(term: &mut Term, d: &ListDisplay) -> Result<()> {
    let mut top = 0usize;
    let mut message = String::new();
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, &d.title);
        let mut row = 3u8;
        for (k, v) in &d.info {
            s.put(row, 2, &format!("{}:   {v}", label(k, 26)));
            row += 1;
        }
        row += 1;
        s.text(row, 2, attr::HIGH, &d.heading);
        let first = row + 1;
        let page = &d.rows[top.min(d.rows.len())..(top + LIST_ROWS).min(d.rows.len())];
        if d.rows.is_empty() {
            s.put(first, 4, "(No entries)");
        }
        for (i, line) in page.iter().enumerate() {
            s.put(first + i as u8, 2, line);
        }
        let more = if top + LIST_ROWS < d.rows.len() {
            "More..."
        } else {
            "Bottom"
        };
        s.put(first + LIST_ROWS as u8, 72, more);
        keys(&mut s, 23, "F3=Exit   F12=Cancel");
        s.message(&message);
        message.clear();
        let inp = term.show(&s).await?;
        match inp.aid {
            Aid::F(3) | Aid::F(12) | Aid::Enter => return Ok(()),
            Aid::RollUp => {
                if top + LIST_ROWS < d.rows.len() {
                    top += LIST_ROWS;
                } else {
                    message = "Already at the bottom of the list.".into();
                }
            }
            Aid::RollDown => {
                if top == 0 {
                    message = "Already at the top of the list.".into();
                } else {
                    top = top.saturating_sub(LIST_ROWS);
                }
            }
            _ => {}
        }
    }
}
