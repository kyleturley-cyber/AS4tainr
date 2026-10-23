//! What a running program shares with its job: the job's database session
//! settings (who holds a record lock, how long to wait for one) and the
//! channel to the job's display (EXFMT, DSPLY with a reply).

use crate::dspf;
use crate::workstn::Frame;
use anyhow::Result;
use tokio_postgres::{Client, NoTls};

/// The database session a program's record changes go to when it is not
/// its own: a trigger program runs in the session (and transaction) of
/// the change that called it — an SQL statement's, or a commitment
/// definition's.
#[derive(Clone)]
pub enum DataSession {
    Sql(std::sync::Arc<Client>),
    Cmt(std::sync::Arc<crate::cmtctl::Definition>),
}

impl DataSession {
    pub fn client(&self) -> &Client {
        match self {
            DataSession::Sql(c) => c,
            DataSession::Cmt(d) => &d.client,
        }
    }
}

/// The tasks driving the database connections still open. A command
/// line process waits for them before it exits ([`close_connections`]).
static CONNECTIONS: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

/// A database session of its own (each program call, each job).
pub async fn connect_db(url: &str) -> Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("as4tainr: database connection error: {e}");
        }
    });
    let mut tasks = CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    tasks.retain(|t| !t.is_finished());
    tasks.push(task);
    Ok(client)
}

/// Lets the connections whose clients are gone finish before the process
/// exits: a transaction dropped on an error (CPF7302 from CRTPF, say) has
/// queued its ROLLBACK, and ending the runtime first would cut the socket
/// while PostgreSQL answers ("could not send data to client: Broken
/// pipe" in the server log). Connections still in use (kept by a static
/// cache) are left after `wait`.
pub async fn close_connections(wait: std::time::Duration) {
    let tasks: Vec<_> = std::mem::take(&mut *CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner()));
    let _ = tokio::time::timeout(wait, futures_util::future::join_all(tasks)).await;
}

/// Set up a job's database session: other jobs see who holds a record
/// lock by application_name; reads wait for locked records up to WAITRCD
/// (lock_timeout).
pub async fn job_session(
    client: &Client,
    job: Option<&str>,
    wait: std::time::Duration,
) -> Result<()> {
    client
        .batch_execute(&format!(
            "SET application_name = '{}'; SET lock_timeout = '{}ms'",
            job.unwrap_or("")
                .replace('\'', "")
                .chars()
                .take(60)
                .collect::<String>(),
            wait.as_millis().max(1)
        ))
        .await?;
    Ok(())
}

/// The record wait: AS4TAINR_WAITRCD seconds, else 60. On IBM i each file
/// has its own WAITRCD (CRTPF, OVRDBF); as4tainr has one wait time for
/// every file (a known gap).
pub fn default_record_wait() -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var("AS4TAINR_WAITRCD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60),
    )
}

/// One screen to show (EXFMT, DSPLY with a reply), sent to whatever owns
/// the job's display; the reply is what the operator entered.
#[derive(Debug)]
pub struct DisplayRequest {
    /// What to show: the records on the screen, the one being read last.
    pub frame: Frame,
    pub reply: tokio::sync::oneshot::Sender<std::result::Result<dspf::Response, String>>,
}

pub type DisplayHandle = tokio::sync::mpsc::Sender<DisplayRequest>;
