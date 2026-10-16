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
use crate::cl;
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

/// A password as typed into a nondisplay field, without the blanks around
/// it. Positions the operator did not type into come back as blanks (or
/// nulls) depending on the emulator: tn5250j sends blanks in front of the
/// password when the cursor was put in the middle of the field. Passwords
/// cannot start or end with a blank, so the blanks are not part of it.
fn password<'a>(inp: &'a ds::Input, field: &str) -> &'a str {
    inp.get(field).trim_matches(|c: char| c == ' ' || c == '\0')
}

/// What a typed secret looked like, without revealing it: its length and
/// the kinds of characters in it. For the server log when sign-on fails.
fn describe_secret(raw: &str) -> String {
    let (mut upper, mut lower, mut digit, mut blank, mut other) = (0, 0, 0, 0, 0);
    for c in raw.chars() {
        match c {
            'A'..='Z' => upper += 1,
            'a'..='z' => lower += 1,
            '0'..='9' => digit += 1,
            ' ' | '\0' => blank += 1,
            _ => other += 1,
        }
    }
    format!(
        "{} characters ({upper} upper case, {lower} lower case, {digit} digits, {blank} blanks, {other} other)",
        raw.chars().count()
    )
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
    eprintln!("ibmish: {device}: terminal type {}", conn.terminal_type);
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
        let profile = match users::authenticate(&client, &user, password(&inp, "PASSWORD")).await {
            Ok(p) => p,
            Err(e) => {
                attempts += 1;
                eprintln!(
                    "ibmish: {device}: sign-on failed for user '{user}' ({}); password received: {}; fields modified: {:?}; cursor {:?}",
                    e,
                    describe_secret(inp.get("PASSWORD")),
                    inp.modified,
                    inp.cursor,
                );
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

        // Current library: from the sign-on screen, else the profile's.
        // *CRTDFT means none.
        let curlib = match inp.get("CURLIB").trim() {
            "" => profile.curlib.clone(),
            c => c.to_ascii_uppercase(),
        };
        let curlib = if curlib == "*CRTDFT" {
            None
        } else {
            if !crate::catalog::library_exists(&client, &curlib)
                .await
                .unwrap_or(false)
            {
                message = format!("CPF2110: Library {curlib} not found.");
                continue;
            }
            Some(curlib)
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
        let level = users::password_level();
        if users::normalize_password(level, password(&inp, "NEW"))
            != users::normalize_password(level, password(&inp, "VERIFY"))
        {
            message = "New password and verify password do not match.".into();
            continue;
        }
        match users::change_password(
            client,
            user,
            password(&inp, "CURRENT"),
            password(&inp, "NEW"),
        )
        .await
        {
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
        let line = match inp.aid {
            Aid::F(3) => return Ok(()),
            Aid::F(12) => continue,
            Aid::F(4) => {
                let typed = inp.get("CMD").trim().to_string();
                if typed.is_empty() {
                    match command_menu(term).await? {
                        Some(line) => line,
                        None => continue,
                    }
                } else {
                    match cl::parse_partial(&typed) {
                        Err(e) => {
                            message = e.to_string();
                            prefill = typed;
                            continue;
                        }
                        Ok((def, vals)) => match prompt(term, def, &vals).await? {
                            PromptResult::Run(line) => line,
                            PromptResult::Exit => continue,
                            PromptResult::Cancel => {
                                prefill = typed;
                                continue;
                            }
                        },
                    }
                }
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
            Aid::Enter => inp.get("CMD").trim().to_string(),
            _ => continue,
        };
        if line.is_empty() {
            continue;
        }
        retrieve.push(line.clone());
        retrieve_pos = 0;
        log.push(format!("> {line}"));
        // Run the command; while it runs, show any display file records
        // its program writes (EXFMT) on this terminal.
        let user = job.user.clone();
        let result = {
            let run = job.run(&line);
            tokio::pin!(run);
            loop {
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
                show_list(term, job, d).await?;
            }
            Ok(Outcome::ChangePassword) => {
                if change_password(term, job.client, &job.user, false).await? {
                    message = "Password changed successfully.".into();
                } else {
                    message = "Password not changed.".into();
                }
                log.push(format!("  {message}"));
            }
            Ok(Outcome::StartSql) => {
                sql_session(term, job).await?;
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

/// What the operator did on a menu.
enum MenuChoice {
    /// A valid option number (0-based index).
    Option(usize),
    /// A command typed on the selection line, and whether F4 was pressed.
    Command(String, bool),
    /// F12.
    Back,
    /// F3.
    Exit,
}

/// A numbered menu with a "Selection or command" line, in the usual IBM i
/// layout. Typing an option number and Enter selects it; a command name
/// runs it (or prompts it with F4).
async fn menu(
    term: &mut Term,
    title: &str,
    items: &[String],
    message: &mut String,
) -> Result<MenuChoice> {
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, title);
        s.put(3, 2, "Select one of the following:");
        for (i, item) in items.iter().enumerate() {
            s.put(5 + i as u8, 6, &format!("{:>2}. {item}", i + 1));
        }
        s.put(19, 2, "Selection or command");
        s.text(20, 2, attr::HIGH, "===>");
        s.field("SEL", 20, 7, 74, FieldKind::Text, "");
        keys(&mut s, 22, "F3=Exit   F4=Prompt   F12=Cancel");
        s.message(message);
        message.clear();
        let inp = term.show(&s).await?;
        let typed = inp.get("SEL").trim().to_string();
        match inp.aid {
            Aid::F(3) => return Ok(MenuChoice::Exit),
            Aid::F(12) => return Ok(MenuChoice::Back),
            Aid::Enter | Aid::F(4) => {}
            _ => {
                *message = "That key is not valid on this display.".into();
                continue;
            }
        }
        if typed.is_empty() {
            if inp.aid == Aid::F(4) {
                *message = "Type an option number or a command name to prompt.".into();
            }
            continue;
        }
        if typed.chars().all(|c| c.is_ascii_digit()) {
            match typed.parse::<usize>() {
                Ok(n) if (1..=items.len()).contains(&n) && inp.aid == Aid::Enter => {
                    return Ok(MenuChoice::Option(n - 1))
                }
                _ => {
                    *message = format!(
                        "Selection {typed} is not valid. Type a number from 1 to {}.",
                        items.len()
                    );
                    continue;
                }
            }
        }
        return Ok(MenuChoice::Command(typed, inp.aid == Aid::F(4)));
    }
}

/// F4 on an empty command line: the command groups, then the commands of
/// a group, then the prompt of the command chosen. Returns the command to
/// run, or None to go back to the command line.
async fn command_menu(term: &mut Term) -> Result<Option<String>> {
    let mut group: Option<usize> = None;
    let mut message = String::new();
    loop {
        let (title, items): (String, Vec<String>) = match group {
            None => (
                "Command Groups".into(),
                cl::GROUPS.iter().map(|g| g.item.to_string()).collect(),
            ),
            Some(g) => {
                let g = &cl::GROUPS[g];
                (
                    g.title.to_string(),
                    g.commands
                        .iter()
                        .map(|name| {
                            let def = cl::find(name).expect("groups list known commands");
                            format!("{:<40}{}", def.description, def.name)
                        })
                        .collect(),
                )
            }
        };
        match menu(term, &title, &items, &mut message).await? {
            MenuChoice::Exit => return Ok(None),
            MenuChoice::Back => match group {
                Some(_) => group = None,
                None => return Ok(None),
            },
            MenuChoice::Option(i) => match group {
                None => group = Some(i),
                Some(g) => {
                    let def =
                        cl::find(cl::GROUPS[g].commands[i]).expect("groups list known commands");
                    match prompt(term, def, &Default::default()).await? {
                        PromptResult::Run(line) => return Ok(Some(line)),
                        PromptResult::Exit => return Ok(None),
                        PromptResult::Cancel => {}
                    }
                }
            },
            MenuChoice::Command(line, false) => return Ok(Some(line)),
            MenuChoice::Command(line, true) => match cl::parse_partial(&line) {
                Err(e) => message = e.to_string(),
                Ok((def, vals)) => match prompt(term, def, &vals).await? {
                    PromptResult::Run(line) => return Ok(Some(line)),
                    PromptResult::Exit => return Ok(None),
                    PromptResult::Cancel => {}
                },
            },
        }
    }
}

enum PromptResult {
    /// Run this command.
    Run(String),
    /// F12: back to the command line with what was typed.
    Cancel,
    /// F3: back to an empty command line.
    Exit,
}

const PROMPT_FIRST_ROW: u8 = 5;
const PROMPT_LABEL_COL: u8 = 2;
const PROMPT_FIELD_COL: u8 = 37;

fn prompt_width(kind: cl::Kind) -> u16 {
    match kind {
        cl::Kind::Name | cl::Kind::Qualified => 10,
        cl::Kind::Password => users::MAX_PASSWORD as u16,
        cl::Kind::Text | cl::Kind::List => 42,
    }
}

/// F4: a screen with one input field per parameter, filled with what was
/// typed or the default. Enter builds the command and checks it.
async fn prompt(
    term: &mut Term,
    def: &'static cl::CommandDef,
    typed: &std::collections::BTreeMap<&'static str, Vec<cl::Value>>,
) -> Result<PromptResult> {
    use std::collections::BTreeMap;
    let initial = || -> BTreeMap<String, String> {
        let mut v = BTreeMap::new();
        for pd in def.params {
            let text = match typed.get(pd.keyword) {
                Some(vals) => cl::prompt_text(pd.kind, vals),
                None => pd.default.unwrap_or("").to_string(),
            };
            if pd.kind == cl::Kind::Qualified {
                let (lib, obj) = text.split_once('/').unwrap_or(("*LIBL", &text));
                v.insert(format!("{}.LIB", pd.keyword), lib.to_string());
                v.insert(pd.keyword.to_string(), obj.to_string());
            } else {
                v.insert(pd.keyword.to_string(), text);
            }
        }
        v
    };
    let mut values = initial();
    let mut message = String::new();
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, &format!("{} ({})", def.description, def.name));
        s.put(3, 2, "Type choices, press Enter.");
        let mut row = PROMPT_FIRST_ROW;
        for pd in def.params {
            let a = if pd.required {
                attr::HIGH
            } else {
                attr::NORMAL
            };
            s.text(row, PROMPT_LABEL_COL, a, &label(pd.prompt, 33));
            let w = prompt_width(pd.kind);
            let kind = match pd.kind {
                cl::Kind::Password => FieldKind::Hidden,
                cl::Kind::Text | cl::Kind::List => FieldKind::Text,
                _ => FieldKind::Upper,
            };
            let v = values.get(pd.keyword).cloned().unwrap_or_default();
            s.field(pd.keyword, row, PROMPT_FIELD_COL, w, kind, &v);
            // Allowed values to the right, when they fit on the line.
            if !pd.choices.is_empty()
                && PROMPT_FIELD_COL as usize + w as usize + 2 + pd.choices.len() <= 80
            {
                s.put(row, PROMPT_FIELD_COL + w as u8 + 2, pd.choices);
            }
            row += 1;
            if pd.kind == cl::Kind::Qualified {
                s.put(row, PROMPT_LABEL_COL + 2, &label("Library", 31));
                let key = format!("{}.LIB", pd.keyword);
                let v = values.get(&key).cloned().unwrap_or_default();
                s.field(&key, row, PROMPT_FIELD_COL, 10, FieldKind::Upper, &v);
                s.put(row, PROMPT_FIELD_COL + 12, "Name, *LIBL, *CURLIB");
                row += 1;
            }
        }
        s.put(22, 74, "Bottom");
        keys(&mut s, 23, "F3=Exit   F5=Refresh   F12=Cancel");
        s.message(&message);
        message.clear();
        let inp = term.show(&s).await?;
        for (name, v) in &inp.values {
            values.insert(name.clone(), v.trim_end().to_string());
        }
        match inp.aid {
            Aid::F(3) => return Ok(PromptResult::Exit),
            Aid::F(12) => return Ok(PromptResult::Cancel),
            Aid::F(5) => {
                values = initial();
                continue;
            }
            Aid::Enter => {}
            _ => {
                message = "That key is not valid on this display.".into();
                continue;
            }
        }
        // Put qualified names back together, then build the command.
        let mut flat = BTreeMap::new();
        for pd in def.params {
            let v = values.get(pd.keyword).cloned().unwrap_or_default();
            if pd.kind == cl::Kind::Qualified {
                let lib = values
                    .get(&format!("{}.LIB", pd.keyword))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_default();
                let v = v.trim();
                let q = if v.is_empty() || lib.is_empty() || lib == "*LIBL" {
                    v.to_string()
                } else {
                    format!("{lib}/{v}")
                };
                flat.insert(pd.keyword.to_string(), q);
            } else {
                flat.insert(pd.keyword.to_string(), v);
            }
        }
        let line = cl::build(def, &flat);
        match cl::parse(&line) {
            Ok(_) => return Ok(PromptResult::Run(line)),
            Err(e) => message = e.to_string(),
        }
    }
}

/// The list area of a list display ends on line 20; line 21 says More...
/// or Bottom. How many lines it has depends on the info lines above it.
const LIST_LAST_ROW: usize = 20;

/// STRSQL: type SQL statements; SELECT results are shown as a list,
/// other statements report how many rows they changed. F9 retrieves the
/// previous statement, F3 ends the session.
async fn sql_session(term: &mut Term, job: &mut Job<'_>) -> Result<()> {
    let mut log: Vec<String> = Vec::new();
    let mut message = String::new();
    let mut retrieve: Vec<String> = Vec::new();
    let mut retrieve_pos = 0usize;
    let mut prefill = String::new();
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, "Enter SQL Statements");
        s.put(3, 2, "Type SQL statement, press Enter.");
        let start = log.len().saturating_sub(14);
        for (i, line) in log[start..].iter().enumerate() {
            let line: String = line.chars().take(78).collect();
            s.put(5 + i as u8, 2, &line);
        }
        s.text(20, 2, attr::HIGH, "===>");
        // Two lines for the statement: the field wraps to line 21.
        s.field("STMT", 20, 7, 74 + 80, FieldKind::Text, &prefill);
        keys(&mut s, 23, "F3=Exit   F9=Retrieve   F12=Cancel");
        s.message(&message);
        message.clear();
        prefill.clear();
        let inp = term.show(&s).await?;
        match inp.aid {
            Aid::F(3) => return Ok(()),
            Aid::F(12) => continue,
            Aid::F(9) => {
                if retrieve.is_empty() {
                    message = "There are no statements to retrieve.".into();
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
            _ => {
                message = "That key is not valid on this display.".into();
                continue;
            }
        }
        let stmt = inp.get("STMT").trim().to_string();
        if stmt.is_empty() {
            continue;
        }
        retrieve.push(stmt.clone());
        retrieve_pos = 0;
        log.push(format!("> {stmt}"));
        match crate::sql::run(
            job.client,
            &job.env.database_url,
            &job.library_list(),
            &stmt,
        )
        .await
        {
            Ok(crate::sql::Outcome::Rows {
                columns,
                numeric,
                rows,
            }) => {
                let (heading, lines) = crate::sql::format_rows(&columns, &numeric, &rows);
                let n = rows.len();
                list_display(
                    term,
                    &ListDisplay {
                        title: "Display Data".into(),
                        info: vec![],
                        heading,
                        rows: lines,
                        ..Default::default()
                    },
                    "",
                )
                .await?;
                message = if n >= crate::sql::MAX_ROWS {
                    format!("{n} rows selected (no more are shown).")
                } else {
                    format!("{n} rows selected.")
                };
            }
            Ok(crate::sql::Outcome::Count(kind, n)) => {
                message = crate::sql::count_message(kind, n);
            }
            Err(e) => {
                message = e.to_string();
                prefill = stmt;
            }
        }
        log.push(format!("  {message}"));
    }
}

/// Columns moved by F19 (Left) and F20 (Right) on wide lists.
const SHIFT: usize = 40;

/// How a list screen was left.
enum ListResult {
    /// Enter or F12: back to the screen before.
    Back,
    /// F3: all the way back to the command line.
    Exit,
    /// Options were typed: run these commands, then show the list again.
    Run(Vec<String>),
}

/// Show a list, then run the commands of the options typed on it and come
/// back to it, as IBM i list screens do. F12 goes back one screen, F3
/// returns to the command line.
async fn show_list(term: &mut Term, job: &mut Job<'_>, d: ListDisplay) -> Result<()> {
    let mut stack = vec![d];
    let mut message = String::new();
    while let Some(top) = stack.last() {
        let result = list_display(term, top, &message).await?;
        message.clear();
        match result {
            ListResult::Exit => return Ok(()),
            ListResult::Back => {
                stack.pop();
            }
            ListResult::Run(cmds) => {
                let mut shown = Vec::new();
                for c in cmds {
                    match job.run(&c).await {
                        Ok(Outcome::Display(d)) => shown.push(d),
                        Ok(Outcome::Messages(m)) => message = m.last().cloned().unwrap_or_default(),
                        Ok(_) => {}
                        Err(e) => message = e.to_string().lines().last().unwrap_or("").to_string(),
                    }
                }
                // Shown in the order the options were typed.
                stack.extend(shown.into_iter().rev());
            }
        }
    }
    Ok(())
}

async fn list_display(term: &mut Term, d: &ListDisplay, first_message: &str) -> Result<ListResult> {
    let mut top = 0usize;
    let mut left = 0usize;
    let has_options = !d.options.is_empty();
    // The Opt column is as wide as the longest option number.
    let opt_width = d
        .options
        .iter()
        .map(|(n, _, _)| n.to_string().len())
        .max()
        .unwrap_or(0) as u16;
    let text_col: u8 = if has_options { 7 } else { 2 };
    let text_width = 80 - text_col as usize - 1;
    let width = d
        .rows
        .iter()
        .chain(std::iter::once(&d.heading))
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0);
    let wide = width > text_width;
    let cut =
        |line: &str, left: usize| -> String { line.chars().skip(left).take(text_width).collect() };
    let mut message = first_message.to_string();
    // Options typed so far, by row, kept while paging.
    let mut typed: std::collections::BTreeMap<usize, String> = Default::default();
    // Title, blank, [options: 2 lines + blank], info lines, blank, heading.
    let info_row = if has_options { 6 } else { 3 };
    let first_row = info_row + d.info.len() + 2;
    let page_rows = (LIST_LAST_ROW + 1).saturating_sub(first_row).max(1);
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, &d.title);
        if has_options {
            s.put(3, 2, "Type options, press Enter.");
            let opts: Vec<String> = d
                .options
                .iter()
                .map(|(n, t, _)| format!("{n}={t}"))
                .collect();
            s.text(4, 4, attr::BLUE, &opts.join("   "));
        }
        let mut row = info_row as u8;
        for (k, v) in &d.info {
            s.put(row, 2, &format!("{}:   {v}", label(k, 26)));
            row += 1;
        }
        row += 1;
        if has_options {
            s.text(row, 2, attr::HIGH, "Opt");
        }
        s.text(row, text_col, attr::HIGH, &cut(&d.heading, left));
        let first = row + 1;
        let end = (top + page_rows).min(d.rows.len());
        if d.rows.is_empty() {
            s.put(first, 4, "(No entries)");
        }
        for (i, idx) in (top.min(d.rows.len())..end).enumerate() {
            let r = first + i as u8;
            if has_options {
                let v = typed.get(&idx).cloned().unwrap_or_default();
                s.field(&format!("OPT{idx}"), r, 3, opt_width, FieldKind::Text, &v);
            }
            s.put(r, text_col, &cut(&d.rows[idx], left));
        }
        let more = if end < d.rows.len() {
            "More..."
        } else {
            "Bottom"
        };
        s.put(LIST_LAST_ROW as u8 + 1, 72, more);
        keys(
            &mut s,
            23,
            if wide {
                "F3=Exit   F12=Cancel   F19=Left   F20=Right"
            } else {
                "F3=Exit   F12=Cancel"
            },
        );
        s.message(&message);
        message.clear();
        let inp = term.show(&s).await?;
        for (name, v) in &inp.values {
            if let Some(idx) = name
                .strip_prefix("OPT")
                .and_then(|n| n.parse::<usize>().ok())
            {
                let v = v.trim();
                if v.is_empty() {
                    typed.remove(&idx);
                } else {
                    typed.insert(idx, v.to_string());
                }
            }
        }
        match inp.aid {
            Aid::F(3) => return Ok(ListResult::Exit),
            Aid::F(12) => return Ok(ListResult::Back),
            Aid::Enter => {
                if typed.is_empty() {
                    return Ok(ListResult::Back);
                }
                let mut cmds = Vec::new();
                let mut bad = None;
                for (idx, v) in &typed {
                    match d.options.iter().find(|(n, _, _)| n.to_string() == *v) {
                        Some((_, _, cmd)) => cmds.push(
                            cmd.replace("{}", d.keys.get(*idx).map(String::as_str).unwrap_or("")),
                        ),
                        None => {
                            bad = Some((*idx, v.clone()));
                            break;
                        }
                    }
                }
                match bad {
                    None => return Ok(ListResult::Run(cmds)),
                    Some((idx, v)) => {
                        message = format!("Option {v} is not valid.");
                        // Show the page with the wrong option.
                        top = idx / page_rows * page_rows;
                    }
                }
            }
            Aid::F(19) if wide => left = left.saturating_sub(SHIFT),
            Aid::F(20) if wide => {
                if left + text_width < width {
                    left += SHIFT;
                } else {
                    message = "Already at the right edge.".into();
                }
            }
            Aid::RollUp => {
                if top + page_rows < d.rows.len() {
                    top += page_rows;
                } else {
                    message = "Already at the bottom of the list.".into();
                }
            }
            Aid::RollDown => {
                if top == 0 {
                    message = "Already at the top of the list.".into();
                } else {
                    top = top.saturating_sub(page_rows);
                }
            }
            _ => {}
        }
    }
}
