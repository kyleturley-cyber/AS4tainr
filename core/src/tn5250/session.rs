//! One interactive 5250 session: sign-on, password change, the initial
//! program and menu, menus, command entry, and list displays. The screen texts are as4tainr's own; the layout follows
//! common 5250 conventions (title on line 1, function keys on line 22 or
//! 23, messages on line 24).

use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::ds::{self, attr, Aid, FieldKind, Screen};
use super::telnet::{opcode, Connection};
use crate::cl;
use crate::commands::{Environment, Job, ListDisplay, Outcome};
use crate::dspf;
use crate::menus;
use crate::runtime;
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
                Ok(()) => eprintln!("as4tainr: {device} ({peer}) session ended"),
                Err(e) => eprintln!("as4tainr: {device} ({peer}) session ended: {e:#}"),
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

/// A title centered as IBM i centers them: (80 - length) / 2 blanks, so
/// "Command Entry" starts in column 34 and "Display Library" in 33
/// (observed on IBM i 7.5, PUB400 screen captures).
fn centered(s: &mut Screen, row: u8, text: &str) {
    s.text(row, center_col(text), attr::HIGH, text);
}

fn center_col(text: &str) -> u8 {
    ((80 - text.chars().count().min(78) as u8) / 2 + 1).max(2)
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
    eprintln!("as4tainr: {device}: terminal type {}", conn.terminal_type);
    let mut term = Term { conn };
    let client = runtime::connect_db(&cfg.env.database_url).await?;
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
            "as4tainr - legacy RPG and 5250 applications in a container",
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
                    "as4tainr: {device}: sign-on failed for user '{user}' ({}); password received: {}; fields modified: {:?}; cursor {:?}",
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
                message = crate::jobs::display_message(&e.to_string());
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
                message = format!("CPF2110: Library {curlib} does not exist.");
                continue;
            }
            Some(curlib)
        };
        // Programs called from this session show their display files here.
        let (display_tx, mut display_rx) = tokio::sync::mpsc::channel::<runtime::DisplayRequest>(1);
        // The session is an interactive job in QINTER.
        let id = crate::jobs::start_interactive(
            &client,
            device,
            &profile.name,
            curlib.as_deref(),
            &cfg.libl,
        )
        .await?;
        crate::jobs::log(
            &client,
            &id,
            "*INFO",
            &format!("Job {id} started in subsystem QINTER."),
        )
        .await;
        let mut job = Job {
            client: &client,
            env: &cfg.env,
            user: profile.name.clone(),
            curlib,
            libl: cfg.libl.clone(),
            display: Some(display_tx),
            id: Some(id.clone()),
            calls: Default::default(),
        };
        let ended = workstation(&mut term, &mut job, cfg, &mut display_rx, &profile).await;
        // The job ends however the session ends (sign-off, F3, the
        // emulator closing).
        crate::jobs::log(&client, &id, "*INFO", &format!("Job {id} ended.")).await;
        crate::jobs::end(&client, &id, ended.is_ok()).await?;
        ended?;
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
            Err(e) => message = crate::jobs::display_message(&e.to_string()),
        }
    }
}

// Command Entry as IBM i 7.5 draws it (PUB400 captures): the log on rows
// 4-15 (commands from column 5, their messages from column 7), "Bottom"
// on row 16, the command line on rows 18-21.
const LOG_TOP: u8 = 4;
const LOG_ROWS: usize = 12;
const COMMAND_LINE_LEN: u16 = 313;

/// A display the operator can come back to: a menu, or Command Entry
/// (CALL QCMD). The session keeps a stack of them.
enum Place {
    Menu(&'static menus::BuiltinMenu),
    CommandEntry,
}

/// What the operator asked for on a menu or on Command Entry.
enum Action {
    /// Run a request (a command line; `?` first: prompt it).
    Run(String),
    /// F3.
    Exit,
    /// F12.
    Cancel,
}

/// The interactive job's requests: the Command Entry log and the commands
/// F9 retrieves.
#[derive(Default)]
struct Requests {
    /// The log: (request level, "> command" or "  message").
    log: Vec<(usize, String)>,
    /// The request level the next request runs at (the displays stack).
    level: usize,
    retrieve: Vec<String>,
    retrieve_pos: usize,
}

impl Requests {
    /// F9: the previous request, going back one more each time.
    fn previous(&mut self) -> Option<String> {
        if self.retrieve.is_empty() {
            return None;
        }
        if self.retrieve_pos == 0 {
            self.retrieve_pos = self.retrieve.len();
        }
        self.retrieve_pos -= 1;
        Some(self.retrieve[self.retrieve_pos].clone())
    }
}

/// How a request ended, for the display that runs it.
struct Done {
    /// For the message line.
    message: String,
    /// The message is in the Command Entry log too (Command Entry does
    /// not repeat it on line 24).
    in_log: bool,
    /// For the command line (the request, when it failed).
    prefill: String,
    next: Next,
}

enum Next {
    Stay,
    Show(Place),
    SignOff,
}

/// After sign-on: the user profile's initial program, then its initial
/// menu (IBM i: INLPGM, then INLMNU; INLMNU(*SIGNOFF) signs off when the
/// initial program ends). F3 on the initial menu signs off.
async fn workstation(
    term: &mut Term,
    job: &mut Job<'_>,
    cfg: &ServerConfig,
    display_rx: &mut tokio::sync::mpsc::Receiver<runtime::DisplayRequest>,
    profile: &users::Profile,
) -> Result<()> {
    let mut rq = Requests::default();
    let mut stack: Vec<Place> = Vec::new();
    let mut message = String::new();
    let mut prefill = String::new();
    if profile.inlmnu != "*SIGNOFF" {
        match menus::find(job.client, &profile.inlmnu, &job.library_list()).await {
            Ok(menus::Menu::Builtin(m)) => stack.push(Place::Menu(m)),
            Ok(menus::Menu::Program { program, .. }) => {
                // The menu program runs after the initial program; when it
                // returns, Command Entry.
                stack.push(Place::CommandEntry);
                prefill = format!("CALL PGM({program})");
            }
            Err(e) => {
                message = format!("{e} Initial menu {} not shown.", profile.inlmnu);
                stack.push(Place::CommandEntry);
            }
        }
    }
    if profile.inlpgm != "*NONE" {
        let line = format!("CALL PGM({})", profile.inlpgm);
        let done = request(term, job, cfg, display_rx, &mut rq, &line).await?;
        match done.next {
            Next::SignOff => return Ok(()),
            Next::Show(p) => stack.push(p),
            Next::Stay => {}
        }
        if !done.message.is_empty() {
            message = done.message;
        }
    }
    // A *PGM initial menu: call it now.
    if !prefill.is_empty() {
        let line = std::mem::take(&mut prefill);
        let done = request(term, job, cfg, display_rx, &mut rq, &line).await?;
        match done.next {
            Next::SignOff => return Ok(()),
            Next::Show(p) => stack.push(p),
            Next::Stay => {}
        }
        message = done.message;
    }
    while let Some(top) = stack.last() {
        let bottom = stack.len() == 1;
        let level = stack.len();
        let action = match top {
            Place::Menu(m) => menu_display(term, m, cfg, &mut rq, &message, &prefill).await?,
            Place::CommandEntry => {
                command_entry(term, cfg, &mut rq, level, &message, &prefill).await?
            }
        };
        message.clear();
        prefill.clear();
        match action {
            Action::Exit => {
                // F3: leave this chain of menus (back to Command Entry or
                // the initial menu); on the initial menu, sign off.
                if bottom {
                    return Ok(());
                }
                match stack.last() {
                    Some(Place::CommandEntry) => {
                        stack.pop();
                    }
                    _ => {
                        while stack.len() > 1 && matches!(stack.last(), Some(Place::Menu(_))) {
                            stack.pop();
                        }
                    }
                }
            }
            Action::Cancel => {
                if !bottom && matches!(top, Place::Menu(_)) {
                    stack.pop();
                }
            }
            Action::Run(line) => {
                rq.level = level;
                let done = request(term, job, cfg, display_rx, &mut rq, &line).await?;
                let on_entry = matches!(stack.last(), Some(Place::CommandEntry));
                if !(done.in_log && on_entry && matches!(done.next, Next::Stay)) {
                    message = done.message;
                }
                prefill = done.prefill;
                match done.next {
                    Next::SignOff => return Ok(()),
                    Next::Show(p) => stack.push(p),
                    Next::Stay => {}
                }
            }
        }
    }
    Ok(())
}

/// The Command Entry display (CALL QCMD), laid out as on IBM i 7.5: the
/// request level, the previous commands with their messages (texts
/// without IDs), and a four-line command line.
async fn command_entry(
    term: &mut Term,
    cfg: &ServerConfig,
    rq: &mut Requests,
    level: usize,
    first_message: &str,
    first_prefill: &str,
) -> Result<Action> {
    let mut message = first_message.to_string();
    let mut prefill = first_prefill.to_string();
    // F10: all requests and messages, of every request level, each
    // command with its level (IBM i 7.5, PUB400); otherwise this level's.
    let mut all = false;
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, "Command Entry");
        s.put(1, 66, &format!("{:>14}", cfg.system_name));
        s.put(2, 60, &format!("Request level:{level:>4}"));
        s.put(
            3,
            2,
            if all {
                "All previous commands and messages:"
            } else {
                "Previous commands and messages:"
            },
        );
        let shown: Vec<String> = rq
            .log
            .iter()
            .filter(|(l, _)| all || *l == level)
            .map(|(l, line)| match line.strip_prefix("> ") {
                Some(cmd) if all => format!("{l:>2} > {cmd}"),
                Some(cmd) => format!("   > {cmd}"),
                None => format!("   {line}"),
            })
            .collect();
        if shown.is_empty() {
            s.put(LOG_TOP + 1, 4, "(No previous commands or messages)");
        }
        let start = shown.len().saturating_sub(LOG_ROWS);
        for (i, line) in shown[start..].iter().enumerate() {
            s.put(LOG_TOP + i as u8, 2, line);
        }
        s.put(LOG_TOP + LOG_ROWS as u8, 74, "Bottom");
        s.put(17, 2, "Type command, press Enter.");
        s.text(18, 2, attr::HIGH, "===>");
        s.field("CMD", 18, 7, COMMAND_LINE_LEN, FieldKind::Text, &prefill);
        keys(
            &mut s,
            22,
            if all {
                "F3=Exit   F4=Prompt   F9=Retrieve   F10=Exclude detailed messages"
            } else {
                "F3=Exit   F4=Prompt   F9=Retrieve   F10=Include detailed messages"
            },
        );
        keys(&mut s, 23, "F12=Cancel");
        s.message(&message);
        prefill.clear();
        message.clear();
        let inp = term.show(&s).await?;
        let typed = inp.get("CMD").trim().to_string();
        match inp.aid {
            Aid::F(3) => return Ok(Action::Exit),
            Aid::F(12) => continue,
            Aid::F(10) => {
                all = !all;
                prefill = typed;
                continue;
            }
            Aid::F(4) => return Ok(Action::Run(format!("?{typed}"))),
            Aid::F(9) => {
                match rq.previous() {
                    Some(l) => prefill = l,
                    None => message = "There are no commands to retrieve.".into(),
                }
                continue;
            }
            Aid::Enter if !typed.is_empty() => return Ok(Action::Run(typed)),
            _ => continue,
        }
    }
}

/// A system menu: its options, and a "Selection or command" line that
/// takes an option number or any command.
async fn menu_display(
    term: &mut Term,
    m: &menus::BuiltinMenu,
    cfg: &ServerConfig,
    rq: &mut Requests,
    first_message: &str,
    first_prefill: &str,
) -> Result<Action> {
    let mut message = first_message.to_string();
    let mut prefill = first_prefill.to_string();
    loop {
        let mut s = Screen::new();
        // IBM i 7.5's menus (PUB400, MAIN): the menu name, the title, and
        // "System:" on line 2.
        s.put(1, 2, m.name);
        centered(&mut s, 1, m.title);
        s.put(2, 62, &format!("System:   {}", cfg.system_name));
        s.put(3, 2, "Select one of the following:");
        let mut row = 5u8;
        let mut last = 0u8;
        for o in m.options {
            // A blank line before the jump to option 90.
            if o.number > last + 1 && last > 0 {
                row += 1;
            }
            s.put(row, 6, &format!("{:>2}. {}", o.number, o.text));
            row += 1;
            last = o.number;
        }
        s.put(19, 2, "Selection or command");
        s.text(20, 2, attr::HIGH, "===>");
        s.field("SEL", 20, 7, 74, FieldKind::Text, &prefill);
        keys(&mut s, 22, "F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel");
        s.message(&message);
        message.clear();
        prefill.clear();
        let inp = term.show(&s).await?;
        let typed = inp.get("SEL").trim().to_string();
        match inp.aid {
            Aid::F(3) => return Ok(Action::Exit),
            Aid::F(12) => return Ok(Action::Cancel),
            Aid::F(9) => {
                match rq.previous() {
                    Some(l) => prefill = l,
                    None => message = "There are no commands to retrieve.".into(),
                }
                continue;
            }
            Aid::Enter | Aid::F(4) => {}
            _ => {
                message = "That key is not valid on this display.".into();
                continue;
            }
        }
        if typed.is_empty() {
            if inp.aid == Aid::F(4) {
                return Ok(Action::Run("?".into()));
            }
            continue;
        }
        if typed.chars().all(|c| c.is_ascii_digit()) {
            let chosen = typed
                .parse::<u8>()
                .ok()
                .and_then(|n| m.options.iter().find(|o| o.number == n));
            match chosen {
                Some(o) if inp.aid == Aid::Enter => return Ok(Action::Run(o.command.to_string())),
                _ => {
                    message = format!("Selection {typed} is not one of the options on this menu.");
                    continue;
                }
            }
        }
        return Ok(Action::Run(if inp.aid == Aid::F(4) {
            format!("?{typed}")
        } else {
            typed
        }));
    }
}

/// Run one request in the interactive job, from a menu or Command Entry:
/// `?` in front prompts the command first (F4 with nothing typed shows the
/// command groups). Displays the command shows (lists, programs' display
/// files, the SQL session) are shown here.
async fn request(
    term: &mut Term,
    job: &mut Job<'_>,
    cfg: &ServerConfig,
    display_rx: &mut tokio::sync::mpsc::Receiver<runtime::DisplayRequest>,
    rq: &mut Requests,
    line: &str,
) -> Result<Done> {
    let mut done = Done {
        message: String::new(),
        in_log: false,
        prefill: String::new(),
        next: Next::Stay,
    };
    // What to prompt, and the message the prompt starts with: F4 (`?`),
    // or a command typed without a required parameter, which an
    // interactive job prompts for instead of running it (IBM i 7.5 on
    // PUB400: CRTDTAARA without TYPE shows the CRTDTAARA prompt with the
    // missing parameter's message on line 24).
    let (to_prompt, first) = match line.strip_prefix('?') {
        Some(typed) => (Some(typed.trim().to_string()), String::new()),
        None => {
            let l = line.trim();
            match cl::parse(l) {
                Err(e)
                    if e.to_string()
                        .lines()
                        .any(|m| crate::jobs::message_id(m) == "CPD0071") =>
                {
                    (
                        Some(l.to_string()),
                        crate::jobs::display_text(&e.to_string()),
                    )
                }
                _ => (None, String::new()),
            }
        }
    };
    let line = match to_prompt {
        None => line.trim().to_string(),
        Some(typed) => {
            let typed = typed.as_str();
            let chosen = if typed.is_empty() {
                command_menu(term).await?
            } else {
                // A command made with CRTCMD: its prompt.
                if let Ok(Some(d)) = job.user_command(typed).await {
                    crate::cmd::prompt_def(&d);
                }
                match cl::parse_partial(typed) {
                    Err(e) => {
                        done.message = crate::jobs::display_text(&e.to_string());
                        done.prefill = typed.to_string();
                        return Ok(done);
                    }
                    Ok((def, vals)) => match prompt(term, def, &vals, &first).await? {
                        PromptResult::Run(line) => Some(line),
                        PromptResult::Exit | PromptResult::Cancel => {
                            // The command as given goes into the log with
                            // CPF6801 (prompting ended by F3 or F12), and
                            // the command line is empty again (observed
                            // on IBM i 7.5, PUB400).
                            let given: std::collections::BTreeMap<String, String> = def
                                .params
                                .iter()
                                .filter_map(|pd| {
                                    vals.get(pd.keyword).map(|v| {
                                        (pd.keyword.to_string(), cl::prompt_text(pd.kind, v))
                                    })
                                })
                                .collect();
                            let shown = cl::build(def, &given).to_ascii_uppercase();
                            let text = "Command prompting was ended by F3 or F12.";
                            rq.log.push((rq.level, format!("> {shown}")));
                            rq.log.push((rq.level, format!("  {text}")));
                            if let Some(id) = &job.id {
                                crate::jobs::log(job.client, id, "*RQS", &shown).await;
                                crate::jobs::log(
                                    job.client,
                                    id,
                                    "*ESCAPE",
                                    &format!("CPF6801: {text}"),
                                )
                                .await;
                            }
                            done.message = text.to_string();
                            done.in_log = true;
                            return Ok(done);
                        }
                    },
                }
            };
            match chosen {
                Some(l) => l,
                None => return Ok(done),
            }
        }
    };
    if line.is_empty() {
        return Ok(done);
    }
    rq.log.push((rq.level, format!("> {line}")));
    rq.retrieve.push(line.clone());
    rq.retrieve_pos = 0;
    if let Some(id) = &job.id {
        crate::jobs::log(job.client, id, "*RQS", &line).await;
        let first = line
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("");
        crate::jobs::set_function(
            job.client,
            id,
            &format!("CMD-{}", first.to_ascii_uppercase()),
            "RUN",
        )
        .await;
    }
    // Run the command; while it runs, show any display file records its
    // program writes (EXFMT) on this terminal.
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
    if let Some(id) = &job.id {
        match &result {
            Ok(Outcome::Messages(msgs)) => {
                for m in msgs {
                    crate::jobs::log(job.client, id, "*INFO", m).await;
                }
            }
            Err(e) => {
                for m in e.to_string().lines() {
                    crate::jobs::log(job.client, id, "*ESCAPE", m).await;
                }
            }
            _ => {}
        }
    }
    match result {
        Ok(Outcome::Messages(msgs)) => {
            if let Some(last) = msgs.last() {
                done.message = crate::jobs::shown_text(last).to_string();
                done.in_log = true;
            }
            let level = rq.level;
            rq.log.extend(
                msgs.iter()
                    .map(|m| (level, format!("  {}", crate::jobs::shown_text(m)))),
            );
        }
        Ok(Outcome::Display(d)) => {
            show_list(term, job, &cfg.system_name, d).await?;
        }
        Ok(Outcome::ChangePassword) => {
            done.message = if change_password(term, job.client, &job.user, false).await? {
                "Password changed successfully.".into()
            } else {
                "Password not changed.".into()
            };
            rq.log.push((rq.level, format!("  {}", done.message)));
            done.in_log = true;
        }
        Ok(Outcome::StartSql) => {
            sql_session(term, job).await?;
        }
        Ok(Outcome::SignOff) => done.next = Next::SignOff,
        Ok(Outcome::CommandEntry) => {
            // CALL QCMD runs at the level below the new Command Entry, so
            // only F10 (all messages) shows it (IBM i 7.5, PUB400).
            done.next = Next::Show(Place::CommandEntry)
        }
        Ok(Outcome::Menu(menus::Menu::Builtin(m))) => done.next = Next::Show(Place::Menu(m)),
        Ok(Outcome::Menu(menus::Menu::Program { program, .. })) => {
            // A *PGM menu: the program shows the menu; when it returns,
            // the menu is done.
            return Box::pin(request(
                term,
                job,
                cfg,
                display_rx,
                rq,
                &format!("CALL PGM({program})"),
            ))
            .await;
        }
        Err(e) => {
            let text = e.to_string();
            let lines: Vec<&str> = text.lines().collect();
            done.message = crate::jobs::display_text(&text);
            done.in_log = true;
            let level = rq.level;
            rq.log.extend(
                lines
                    .iter()
                    .map(|m| (level, format!("  {}", crate::jobs::shown_text(m)))),
            );
            done.prefill = line.clone();
        }
    }
    if let Some(id) = &job.id {
        crate::jobs::set_function(job.client, id, "", "DSPW").await;
    }
    // New messages on the user's message queue (a submitted job ended,
    // SNDMSG from another user): say so when nothing else is.
    let waiting = crate::jobs::new_messages(job.client, &job.user)
        .await
        .unwrap_or(0);
    if waiting > 0 && done.message.is_empty() {
        done.message = if waiting == 1 {
            "A new message is on your message queue. Type DSPMSG to see it.".into()
        } else {
            format!("{waiting} new messages are on your message queue. Type DSPMSG to see them.")
        };
    }
    Ok(done)
}

/// Show an EXFMT frame until the operator enters valid input. Paging
/// through a subfile happens here, without the program.
async fn exfmt(
    term: &mut Term,
    req: &runtime::DisplayRequest,
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
    // Lines 5-17 hold the options; a longer menu pages (More... on line
    // 18, Page Down and Page Up).
    const MENU_LINES: usize = 13;
    let pages = items.len().div_ceil(MENU_LINES).max(1);
    let mut page = 0usize;
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, title);
        s.put(3, 2, "Select one of the following:");
        for (i, item) in items
            .iter()
            .enumerate()
            .skip(page * MENU_LINES)
            .take(MENU_LINES)
        {
            s.put(
                5 + (i - page * MENU_LINES) as u8,
                6,
                &format!("{:>2}. {item}", i + 1),
            );
        }
        if pages > 1 {
            if page + 1 < pages {
                s.put(18, 73, "More...");
            } else {
                s.put(18, 74, "Bottom");
            }
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
            Aid::RollUp => {
                if page + 1 < pages {
                    page += 1;
                } else {
                    *message = "Already at bottom of area.".into();
                }
                continue;
            }
            Aid::RollDown => {
                if page > 0 {
                    page -= 1;
                } else {
                    *message = "Already at top of area.".into();
                }
                continue;
            }
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
                    match prompt(term, def, &Default::default(), "").await? {
                        PromptResult::Run(line) => return Ok(Some(line)),
                        PromptResult::Exit => return Ok(None),
                        PromptResult::Cancel => {}
                    }
                }
            },
            MenuChoice::Command(line, false) => return Ok(Some(line)),
            MenuChoice::Command(line, true) => match cl::parse_partial(&line) {
                Err(e) => message = crate::jobs::display_text(&e.to_string()),
                Ok((def, vals)) => match prompt(term, def, &vals, "").await? {
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

// The command prompt as IBM i 7.5 draws it (PUB400 captures): prompt
// texts from column 2 with dots to column 33, the keyword in column 35,
// the field in column 50 (a qualifier's in 52), "Bottom" on row 21.
const PROMPT_FIRST_ROW: u8 = 5;
const PROMPT_LAST_ROW: u8 = 20;
/// Lines of parameters on one prompt page (5-20).
const PROMPT_LINES: usize = 16;
const PROMPT_LABEL_COL: u8 = 2;
const PROMPT_KEYWORD_COL: u8 = 35;
const PROMPT_FIELD_COL: u8 = 50;

/// A prompt text (indented by `indent`) with dots in the odd columns up to
/// column 33, as on IBM i's command prompts.
fn prompt_label(text: &str, indent: usize) -> String {
    let mut s = format!("{}{}", " ".repeat(indent), text);
    // Columns are 1-based from PROMPT_LABEL_COL (2): s[k] is column k + 2.
    let mut col = s.chars().count() + 2 + 1; // a blank after the text
    if col % 2 == 0 {
        col += 1;
    }
    while s.chars().count() + 2 < col {
        s.push(' ');
    }
    while col <= 33 {
        s.push('.');
        if col < 33 {
            s.push(' ');
        }
        col += 2;
    }
    s
}

/// Rows a parameter takes on the prompt: its field from column 50 may go
/// on to the next rows; a qualified name has a row for its library.
fn prompt_rows(pd: &cl::ParamDef) -> usize {
    let end = PROMPT_FIELD_COL as usize - 1 + prompt_width(pd) as usize - 1;
    end / 80 + 1 + usize::from(pd.kind == cl::Kind::Qualified)
}

fn prompt_width(pd: &cl::ParamDef) -> u16 {
    match pd.kind {
        cl::Kind::Name | cl::Kind::Qualified => 10,
        cl::Kind::Password => users::MAX_PASSWORD as u16,
        // TEXT is 50 characters (it goes on to the next row, as on IBM i);
        // other text values to the end of the row.
        cl::Kind::Text if pd.keyword == "TEXT" => 50,
        cl::Kind::Text | cl::Kind::List => 30,
        cl::Kind::Command => 190,
    }
}

/// Parameters IBM i's prompt shows only after F10 (Additional
/// parameters): AUT on the create commands (CRTDTAARA on PUB400).
fn additional_parameter(pd: &cl::ParamDef) -> bool {
    pd.keyword == "AUT"
}

/// Prompt control: a parameter shown only once another one has a value
/// (IBM i 7.5, PUB400: CRTDTAARA shows LEN, VALUE and TEXT after TYPE).
fn prompted_after(command: &str, keyword: &str) -> Option<&'static str> {
    match (command, keyword) {
        ("CRTDTAARA", "LEN" | "VALUE" | "TEXT") => Some("TYPE"),
        _ => None,
    }
}

/// The library a qualified name's prompt starts with: the create commands
/// put new objects in the current library (CRTDTAARA's prompt on PUB400
/// shows *CURLIB), the others look in the library list.
fn default_library(def: &cl::CommandDef) -> &'static str {
    if def.name.starts_with("CRT") {
        "*CURLIB"
    } else {
        "*LIBL"
    }
}

/// What one prompt page shows.
#[derive(Clone, Copy)]
enum PromptItem {
    Param(&'static cl::ParamDef),
    /// "Additional Parameters", after F10.
    Additional,
}

/// F4: a screen with one input field per parameter, filled with what was
/// typed or the default. Enter builds the command and checks it.
async fn prompt(
    term: &mut Term,
    def: &'static cl::CommandDef,
    typed: &std::collections::BTreeMap<&'static str, Vec<cl::Value>>,
    first_message: &str,
) -> Result<PromptResult> {
    use std::collections::BTreeMap;
    let lib_default = default_library(def);
    let initial = || -> BTreeMap<String, String> {
        let mut v = BTreeMap::new();
        for pd in def.params {
            let text = match typed.get(pd.keyword) {
                Some(vals) => cl::prompt_text(pd.kind, vals),
                None => pd.default.unwrap_or("").to_string(),
            };
            if pd.kind == cl::Kind::Qualified {
                let (lib, obj) = text.split_once('/').unwrap_or((lib_default, &text));
                v.insert(format!("{}.LIB", pd.keyword), lib.to_string());
                v.insert(pd.keyword.to_string(), obj.to_string());
            } else {
                v.insert(pd.keyword.to_string(), text);
            }
        }
        v
    };
    let mut values = initial();
    let start_values = initial();
    let mut message = first_message.to_string();
    let mut show_additional = false;
    // The parameters shown: those prompt control does not hold back, the
    // additional ones after F10 under their heading.
    let visible = |values: &BTreeMap<String, String>, additional: bool| -> Vec<PromptItem> {
        let shown = |pd: &cl::ParamDef| {
            prompted_after(def.name, pd.keyword)
                .is_none_or(|k| values.get(k).is_some_and(|v| !v.trim().is_empty()))
        };
        let mut items: Vec<PromptItem> = def
            .params
            .iter()
            .filter(|pd| !additional_parameter(pd) && shown(pd))
            .map(PromptItem::Param)
            .collect();
        if additional {
            let more: Vec<PromptItem> = def
                .params
                .iter()
                .filter(|pd| additional_parameter(pd) && shown(pd))
                .map(PromptItem::Param)
                .collect();
            if !more.is_empty() {
                items.push(PromptItem::Additional);
                items.extend(more);
            }
        }
        items
    };
    let has_additional = def.params.iter().any(additional_parameter);
    let item_rows = |it: &PromptItem| match it {
        PromptItem::Param(pd) => prompt_rows(pd),
        PromptItem::Additional => 3,
    };
    // Pages of parameters: lines 5-20; the Page Down and Page Up keys
    // (Roll Up, Roll Down) move between them.
    let paginate = |items: &[PromptItem]| -> Vec<Vec<PromptItem>> {
        let mut pages: Vec<Vec<PromptItem>> = vec![vec![]];
        let mut used = 0;
        for it in items {
            let n = item_rows(it);
            if used + n > PROMPT_LINES {
                pages.push(vec![]);
                used = 0;
            }
            pages.last_mut().unwrap().push(*it);
            used += n;
        }
        pages
    };
    let mut items = visible(&values, show_additional);
    let mut pages = paginate(&items);
    // The page and field the message is about: a parameter's keyword in
    // the message puts the cursor on that parameter, as IBM i does.
    let mut page = 0usize;
    let mut focus: Option<&'static str> = None;
    let mut focus_next: Option<&'static str> = None;
    loop {
        if let Some(k) = focus_next.take() {
            focus = Some(k);
        }
        if let Some(pd) = def.params.iter().find(|pd| {
            message
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == pd.keyword)
        }) {
            if let Some(p) = pages.iter().position(|pg| {
                pg.iter()
                    .any(|x| matches!(x, PromptItem::Param(p) if p.keyword == pd.keyword))
            }) {
                focus = Some(pd.keyword);
                page = p;
            }
        }
        page = page.min(pages.len() - 1);
        let mut s = Screen::new();
        centered(&mut s, 1, &format!("{} ({})", def.description, def.name));
        s.put(3, 2, "Type choices, press Enter.");
        let mut row = PROMPT_FIRST_ROW;
        let mut cursor = None;
        for it in pages[page].iter().copied() {
            let pd = match it {
                PromptItem::Param(pd) => pd,
                PromptItem::Additional => {
                    centered(&mut s, row + 1, "Additional Parameters");
                    row += 3;
                    continue;
                }
            };
            let a = if pd.required {
                attr::HIGH
            } else {
                attr::NORMAL
            };
            // Prompt text, keyword, '>' before a value that was typed,
            // field: IBM i 7.5's prompt with keywords shown (PUB400).
            s.text(row, PROMPT_LABEL_COL, a, &prompt_label(pd.prompt, 0));
            s.put(row, PROMPT_KEYWORD_COL, pd.keyword);
            // '>': a value given on the command line or changed on the
            // prompt (IBM i 7.5, PUB400).
            let given = typed.contains_key(pd.keyword)
                || values.get(pd.keyword) != start_values.get(pd.keyword);
            if given {
                s.put(row, PROMPT_FIELD_COL - 2, ">");
            }
            let w = prompt_width(pd);
            let kind = match pd.kind {
                cl::Kind::Password => FieldKind::Hidden,
                cl::Kind::Text | cl::Kind::List | cl::Kind::Command => FieldKind::Text,
                _ => FieldKind::Upper,
            };
            let v = values.get(pd.keyword).cloned().unwrap_or_default();
            s.field(pd.keyword, row, PROMPT_FIELD_COL, w, kind, &v);
            if focus == Some(pd.keyword) {
                cursor = Some((row, PROMPT_FIELD_COL));
            }
            row += prompt_rows(pd) as u8;
            if pd.kind == cl::Kind::Qualified {
                row -= 1;
                s.put(row, PROMPT_LABEL_COL, &prompt_label("Library", 2));
                let key = format!("{}.LIB", pd.keyword);
                let lib_given = typed
                    .get(pd.keyword)
                    .is_some_and(|v| cl::prompt_text(pd.kind, v).contains('/'))
                    || values.get(&key) != start_values.get(&key);
                if lib_given {
                    s.put(row, PROMPT_FIELD_COL - 2, ">");
                }
                let v = values.get(&key).cloned().unwrap_or_default();
                s.field(&key, row, PROMPT_FIELD_COL + 2, 10, FieldKind::Upper, &v);
                row += 1;
            }
        }
        if let Some((r, c)) = cursor {
            s.cursor_at(r, c);
        }
        if page + 1 < pages.len() {
            s.put(PROMPT_LAST_ROW + 1, 73, "More...");
        } else {
            s.put(PROMPT_LAST_ROW + 1, 74, "Bottom");
        }
        keys(
            &mut s,
            22,
            if has_additional && !show_additional {
                "F3=Exit   F5=Refresh   F10=Additional parameters   F12=Cancel"
            } else {
                "F3=Exit   F5=Refresh   F12=Cancel"
            },
        );
        s.message(&message);
        message.clear();
        focus = None;
        let inp = term.show(&s).await?;
        for (name, v) in &inp.values {
            values.insert(name.clone(), v.trim_end().to_string());
        }
        match inp.aid {
            Aid::F(3) => return Ok(PromptResult::Exit),
            Aid::F(12) => return Ok(PromptResult::Cancel),
            Aid::F(5) => {
                values = initial();
                items = visible(&values, show_additional);
                pages = paginate(&items);
                continue;
            }
            Aid::F(10) if has_additional && !show_additional => {
                show_additional = true;
                items = visible(&values, show_additional);
                pages = paginate(&items);
                continue;
            }
            Aid::RollUp => {
                if page + 1 < pages.len() {
                    page += 1;
                } else {
                    message = "Already at bottom of area.".into();
                }
                continue;
            }
            Aid::RollDown => {
                if page > 0 {
                    page -= 1;
                } else {
                    message = "Already at top of area.".into();
                }
                continue;
            }
            Aid::Enter => {}
            _ => {
                message = "That key is not valid on this display.".into();
                continue;
            }
        }
        // Values that bring more parameters in (prompt control): show them
        // before running, as IBM i does.
        let now = visible(&values, show_additional);
        if now.len() != items.len() {
            // The cursor goes to the first parameter that came in.
            let key = |it: &PromptItem| match it {
                PromptItem::Param(pd) => pd.keyword,
                PromptItem::Additional => "",
            };
            let before: Vec<&str> = items.iter().map(key).collect();
            if let Some(PromptItem::Param(pd)) = now.iter().find(|it| !before.contains(&key(it))) {
                focus_next = Some(pd.keyword);
            }
            items = now;
            pages = paginate(&items);
            continue;
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
                let q = if v.is_empty() || lib.is_empty() || lib == lib_default {
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
            Err(e) => message = crate::jobs::display_text(&e.to_string()),
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
    // SET SCHEMA in the session: the default schema for what follows.
    let mut schema: Option<String> = None;
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
        match crate::ddl::try_run(
            &job.env.database_url,
            &job.user,
            &job.library_list(),
            Some(job.current_library()),
            &mut schema,
            &stmt,
        )
        .await
        {
            Ok(Some(msg)) => {
                message = msg;
                log.push(format!("  {message}"));
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                message = crate::jobs::display_text(&e.to_string());
                prefill = stmt;
                log.push(format!("  {message}"));
                continue;
            }
        }
        let libl = match &schema {
            Some(s) => vec![s.clone()],
            None => job.library_list(),
        };
        match crate::sql::run_as(
            job.client,
            &job.env.database_url,
            &libl,
            &job.user,
            Some(job.current_library()),
            job.id.as_ref().map(|i| i.to_string()),
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
            Ok(crate::sql::Outcome::Message(m)) => message = m,
            Err(e) => {
                message = crate::jobs::display_text(&e.to_string());
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
    /// F5: show the list again with fresh data.
    Refresh,
}

/// Show a list, then run the commands of the options typed on it and come
/// back to it, as IBM i list screens do. F12 goes back one screen, F3
/// returns to the command line.
async fn show_list(term: &mut Term, job: &mut Job<'_>, system: &str, d: ListDisplay) -> Result<()> {
    let mut stack = vec![d];
    let mut message = String::new();
    while let Some(top) = stack.last() {
        let result = list_display(term, top, system, &message).await?;
        message.clear();
        match result {
            ListResult::Exit => return Ok(()),
            ListResult::Back => {
                stack.pop();
            }
            ListResult::Refresh => refresh_top(job, &mut stack, &mut message).await,
            ListResult::Run(cmds) => {
                let mut shown = Vec::new();
                for c in cmds {
                    match job.run(&c).await {
                        Ok(Outcome::Display(d)) => shown.push(d),
                        Ok(Outcome::Messages(m)) => message = m.last().cloned().unwrap_or_default(),
                        Ok(_) => {}
                        Err(e) => message = crate::jobs::display_text(&e.to_string()),
                    }
                }
                // Options change what the list shows (ENDJOB, DLTSPLF, ...).
                let kept = message.clone();
                refresh_top(job, &mut stack, &mut message).await;
                if message.is_empty() {
                    message = kept;
                }
                // Shown in the order the options were typed.
                stack.extend(shown.into_iter().rev());
            }
        }
    }
    Ok(())
}

/// Run the top list's refresh command and show its new data.
async fn refresh_top(job: &mut Job<'_>, stack: &mut [ListDisplay], message: &mut String) {
    let Some(cmd) = stack.last().and_then(|d| d.refresh.clone()) else {
        return;
    };
    match job.run(&cmd).await {
        Ok(Outcome::Display(d)) => {
            if let Some(top) = stack.last_mut() {
                *top = d;
            }
        }
        Ok(_) => {}
        Err(e) => *message = crate::jobs::display_text(&e.to_string()),
    }
}

async fn list_display(
    term: &mut Term,
    d: &ListDisplay,
    system: &str,
    first_message: &str,
) -> Result<ListResult> {
    if let Some(l) = &d.ibm {
        return ibm_list_display(term, d, l, system, first_message).await;
    }
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
            if has_options && d.keys.get(idx).is_some_and(|k| !k.is_empty()) {
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
            match (d.refresh.is_some(), wide) {
                (true, true) => "F3=Exit   F5=Refresh   F12=Cancel   F19=Left   F20=Right",
                (true, false) => "F3=Exit   F5=Refresh   F12=Cancel",
                (false, true) => "F3=Exit   F12=Cancel   F19=Left   F20=Right",
                (false, false) => "F3=Exit   F12=Cancel",
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
            Aid::F(5) if d.refresh.is_some() => return Ok(ListResult::Refresh),
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

/// Function key texts on as many lines as they need (at most 78
/// characters each), as IBM i's displays show them.
fn key_lines(keys: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new()];
    for k in keys {
        let last = lines.last_mut().unwrap();
        if !last.is_empty() && last.len() + 3 + k.len() > 78 {
            lines.push(k.to_string());
        } else {
            if !last.is_empty() {
                last.push_str("   ");
            }
            last.push_str(k);
        }
    }
    lines
}

/// A list display in IBM i's layout (`commands::IbmLayout`).
async fn ibm_list_display(
    term: &mut Term,
    d: &ListDisplay,
    l: &crate::commands::IbmLayout,
    system: &str,
    first_message: &str,
) -> Result<ListResult> {
    let has_options = !d.options.is_empty();
    let opt_width = d
        .options
        .iter()
        .map(|(n, _, _)| n.to_string().len())
        .max()
        .unwrap_or(1) as u16;
    // The option field ends in column 3 (column 2 for two digits).
    let opt_col = (4 - opt_width.min(2)) as u8;
    let text_col: u8 = if has_options { 7 } else { 2 };
    // Up to column 79 (IBM i leaves column 80 blank).
    let text_width = 80 - text_col as usize;
    let width = d
        .rows
        .iter()
        .chain(std::iter::once(&d.heading))
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0);
    let wide = width > text_width;
    let mut fkeys: Vec<&str> = vec!["F3=Exit"];
    if d.refresh.is_some() {
        fkeys.push("F5=Refresh");
    }
    fkeys.push("F12=Cancel");
    if l.view2.is_some() {
        fkeys.push("F11=Next view");
    }
    if l.top_bottom_keys {
        fkeys.push("F17=Top");
        fkeys.push("F18=Bottom");
    }
    if wide {
        fkeys.push("F19=Left");
        fkeys.push("F20=Right");
    }
    let key_text = key_lines(&fkeys);
    let key_top = 24 - key_text.len() as u8;
    let bottom_row = if l.command_line.is_some() || l.enter_line {
        key_top - 3
    } else {
        key_top - 1
    };
    // Where the list starts: the header lines, a blank line, the options
    // (or an instruction) and a blank line, the headings.
    let mut first = 3u8;
    if !l.lines.is_empty() {
        first += l.lines.len() as u8 + 1;
    }
    let option_lines: Vec<String> = {
        let mut out = vec![String::new()];
        for (n, t, _) in &d.options {
            let o = format!("{n}={t}");
            let last = out.last_mut().unwrap();
            if !last.is_empty() && 3 + last.len() + 3 + o.len() > 78 {
                out.push(o);
            } else {
                if !last.is_empty() {
                    last.push_str("   ");
                }
                last.push_str(&o);
            }
        }
        out
    };
    let options_row = first;
    if has_options {
        first += 1 + option_lines.len() as u8 + 1;
    } else if !l.instruction.is_empty() {
        first += 1;
    }
    if !l.heading2.is_empty() {
        first += 1;
    }
    if !d.heading.is_empty() {
        first += 1;
    }
    let page_rows = (bottom_row.saturating_sub(first)).max(1) as usize;
    let mut top = 0usize;
    let mut left = 0usize;
    let cut =
        |line: &str, left: usize| -> String { line.chars().skip(left).take(text_width).collect() };
    let mut message = first_message.to_string();
    let mut typed: std::collections::BTreeMap<usize, String> = Default::default();
    let mut command = String::new();
    let mut second = false;
    loop {
        let (heading2, heading, rows) = match (&l.view2, second) {
            (Some((h2, h, r)), true) => (h2, h, r),
            _ => (&l.heading2, &d.heading, &d.rows),
        };
        let mut s = Screen::new();
        centered(&mut s, 1, &d.title);
        if let Some(col) = l.system_col {
            s.put(2, col, &format!("System:   {system}"));
        }
        if !l.right2.is_empty() {
            s.put(2, 80 - l.right2.chars().count() as u8, &l.right2);
        }
        for (i, line) in l.lines.iter().enumerate() {
            if !line.trim().is_empty() {
                s.put(3 + i as u8, 2, line);
            }
        }
        let mut row = options_row;
        if has_options {
            s.put(row, 2, "Type options, press Enter.");
            for (i, o) in option_lines.iter().enumerate() {
                s.text(row + 1 + i as u8, 4, attr::BLUE, o);
            }
            row += 1 + option_lines.len() as u8 + 1;
        } else if !l.instruction.is_empty() {
            s.put(row, 2, &l.instruction);
            row += 1;
        }
        if !l.heading2.is_empty() {
            s.text(row, text_col, attr::HIGH, &cut(heading2, left));
            row += 1;
        }
        if !d.heading.is_empty() {
            if has_options {
                s.text(row, 2, attr::HIGH, "Opt");
            }
            s.text(row, text_col, attr::HIGH, &cut(heading, left));
        }
        let end = (top + page_rows).min(d.rows.len());
        if d.rows.is_empty() {
            s.put(first, 4, "(No entries)");
        }
        for (i, idx) in (top.min(d.rows.len())..end).enumerate() {
            let r = first + i as u8;
            if has_options && d.keys.get(idx).is_some_and(|k| !k.is_empty()) {
                let v = typed.get(&idx).cloned().unwrap_or_default();
                s.field(
                    &format!("OPT{idx}"),
                    r,
                    opt_col,
                    opt_width,
                    FieldKind::Text,
                    &v,
                );
            }
            s.put(
                r,
                text_col,
                &cut(rows.get(idx).map(String::as_str).unwrap_or(""), left),
            );
        }
        if end < d.rows.len() {
            s.put(bottom_row, 73, "More...");
        } else {
            s.put(bottom_row, 74, "Bottom");
        }
        if let Some(text) = &l.command_line {
            s.put(key_top - 2, 2, text);
            s.text(key_top - 1, 2, attr::HIGH, "===>");
            s.field("CMD", key_top - 1, 7, 73, FieldKind::Text, &command);
        } else if l.enter_line {
            s.put(key_top - 2, 2, "Press Enter to continue.");
        }
        for (i, k) in key_text.iter().enumerate() {
            keys(&mut s, key_top + i as u8, k);
        }
        if !has_options && l.command_line.is_none() {
            s.cursor_at(1, 2);
        }
        s.message(&message);
        message.clear();
        let inp = term.show(&s).await?;
        for (name, v) in &inp.values {
            if name == "CMD" {
                command = v.trim().to_string();
            } else if let Some(idx) = name
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
        let last_page = d.rows.len().saturating_sub(1) / page_rows * page_rows;
        match inp.aid {
            Aid::F(3) => return Ok(ListResult::Exit),
            Aid::F(12) => return Ok(ListResult::Back),
            Aid::F(5) if d.refresh.is_some() => return Ok(ListResult::Refresh),
            Aid::F(11) if l.view2.is_some() => second = !second,
            Aid::F(17) if l.top_bottom_keys => top = 0,
            Aid::F(18) if l.top_bottom_keys => top = last_page,
            Aid::Enter => {
                if typed.is_empty() && command.is_empty() {
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
                    None => {
                        if !command.is_empty() {
                            cmds.push(std::mem::take(&mut command));
                        }
                        return Ok(ListResult::Run(cmds));
                    }
                    Some((idx, v)) => {
                        message = format!("Option {v} is not valid.");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_labels_and_titles_as_ibm_i_draws_them() {
        // Columns observed on IBM i 7.5 (PUB400, CRTDTAARA and CHGDTAARA
        // prompts): dots in the odd columns from the first one after a
        // blank, up to column 33; the label starts in column 2.
        let col_of_first_dot = |l: &str| l.find('.').unwrap() + PROMPT_LABEL_COL as usize;
        let data_area = prompt_label("Data area", 0);
        assert_eq!(col_of_first_dot(&data_area), 13);
        assert_eq!(data_area.len() + 1, 33);
        assert_eq!(col_of_first_dot(&prompt_label("Library", 2)), 13);
        assert_eq!(col_of_first_dot(&prompt_label("Type", 0)), 7);
        assert_eq!(col_of_first_dot(&prompt_label("Data area", 2)), 15);
        assert_eq!(
            col_of_first_dot(&prompt_label("Substring starting position", 2)),
            33
        );
        // Titles: "Command Entry" in column 34, "Display Library" in 33,
        // "Create Data Area (CRTDTAARA)" in 27.
        for (title, col) in [
            ("Command Entry", 34u8),
            ("Display Library", 33),
            ("Create Data Area (CRTDTAARA)", 27),
            ("Display Data Area", 32),
        ] {
            assert_eq!(center_col(title), col, "{title}");
        }
    }
}
