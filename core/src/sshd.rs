//! The SSH server: what Code for IBM i (and ssh, scp and sftp) connect
//! to. IBM i's is OpenSSH in PASE; as4tainr's speaks the same protocol
//! (the russh library) with as4tainr's user profiles and the IFS:
//!
//! - sign-on with a user profile's password (as the 5250 sign-on checks
//!   it: users.rs `authenticate`); a profile whose password has expired
//!   or that is disabled is refused;
//! - a command (`ssh as4tainr 'ls /home'`) runs with a shell in the
//!   container, on the IFS at its paths when the system allows mount
//!   namespaces (qshfs.rs, as QSH runs), as the profile: HOME is its IFS
//!   home directory, `system` runs CL commands for it, and the PASE
//!   paths Code for IBM i uses exist (/QOpenSys/usr/bin/system, the
//!   /QOpenSys/pkgs/bin tools, `uname` answering as PASE does, a Java
//!   command at /QOpenSys/QIBM/ProdData/JavaVM/jdkNN/64bit/bin/java);
//! - the Mapepire server started over SSH (`java ... -jar ... --single`,
//!   how Code for IBM i runs its SQL) is answered by as4tainr's own
//!   (mapepire.rs) on the channel;
//! - the sftp subsystem reads and writes the IFS (as4tainr.ifs), with
//!   the owner's and the public's permission bits checked for the
//!   profile (*ALLOBJ passes).
//!
//! It is off unless AS4TAINR_SSH_LISTEN names an address: its commands
//! run outside as4tainr's object authorities, as QSH's do (docs/ssh.md).
//! The host key is made once and kept in the database
//! (as4tainr.ssh_host_keys). What it does not do is in
//! docs/fidelity-gaps.md ("SSH").

use anyhow::{anyhow, Result};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use russh_sftp::server::StatusReply;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.ssh_host_keys (
    algorithm text PRIMARY KEY,
    key       text NOT NULL
);
"#;

/// The PASE shell the sessions get (bash, as IBM i's open-source
/// package installs it).
pub const SHELL: &str = "/QOpenSys/pkgs/bin/bash";

/// Serve SSH on `listen` until the process ends.
pub async fn run(database_url: String, listen: String) -> Result<()> {
    let client = crate::runtime::connect_db(&database_url).await?;
    crate::catalog::init(&client).await?;
    client.batch_execute(DDL).await?;
    let key = host_key(&client).await?;
    if let Err(e) = install_pase_tree() {
        eprintln!("as4tainr: the PASE programs in /QOpenSys were not all made: {e:#}");
    }
    let config = russh::server::Config {
        keys: vec![key],
        auth_rejection_time: std::time::Duration::from_secs(1),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        inactivity_timeout: None,
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        ..Default::default()
    };
    let mut server = Server {
        database_url: Arc::new(database_url),
    };
    eprintln!("as4tainr: SSH server listening on {listen}");
    server
        .run_on_address(Arc::new(config), listen.as_str())
        .await?;
    Ok(())
}

/// The server's Ed25519 host key, made the first time.
async fn host_key(client: &Client) -> Result<russh::keys::PrivateKey> {
    use russh::keys::ssh_key::LineEnding;
    if let Some(r) = client
        .query_opt(
            "SELECT key FROM as4tainr.ssh_host_keys WHERE algorithm = 'ssh-ed25519'",
            &[],
        )
        .await?
    {
        let text: String = r.get(0);
        return russh::keys::PrivateKey::from_openssh(text.as_bytes())
            .map_err(|e| anyhow!("the SSH host key is not valid: {e}"));
    }
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
        .map_err(|e| anyhow!("no SSH host key: {e}"))?;
    let text = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| anyhow!("no SSH host key: {e}"))?
        .to_string();
    client
        .execute(
            "INSERT INTO as4tainr.ssh_host_keys (algorithm, key) VALUES ('ssh-ed25519', $1)
             ON CONFLICT DO NOTHING",
            &[&text],
        )
        .await?;
    // Another server may have made one first: use what is kept.
    let kept: String = client
        .query_one(
            "SELECT key FROM as4tainr.ssh_host_keys WHERE algorithm = 'ssh-ed25519'",
            &[],
        )
        .await?
        .get(0);
    russh::keys::PrivateKey::from_openssh(kept.as_bytes())
        .map_err(|e| anyhow!("the SSH host key is not valid: {e}"))
}

// ------------------------------------------------------- the PASE files

/// The programs at PASE's paths: (path, script) or (path, the
/// container's program it links to).
fn pase_files() -> Vec<(String, PaseFile)> {
    let mut v: Vec<(String, PaseFile)> = Vec::new();
    let exe = "\"${AS4TAINR_EXE:-as4tainr}\"";
    v.push((
        "/QOpenSys/usr/bin/system".into(),
        PaseFile::Script(format!("#!/bin/sh\nexec {exe} system \"$@\"\n")),
    ));
    // PASE's uname: system name OS400, release 5 and version 7 (IBM i
    // 7.5; Code for IBM i reads `uname -rv` as "<release> <version>").
    v.push((
        "/QOpenSys/usr/bin/uname".into(),
        PaseFile::Script(UNAME.into()),
    ));
    for (name, sub) in [("attr", "attr"), ("setccsid", "setccsid")] {
        let body = format!("#!/bin/sh\nexec {exe} {sub} \"$@\"\n");
        v.push((
            format!("/QOpenSys/usr/bin/{name}"),
            PaseFile::Script(body.clone()),
        ));
        // Code for IBM i looks for them in /usr/bin (PASE's): there too,
        // unless the container has a program of that name.
        v.push((format!("/usr/bin/{name}"), PaseFile::IfAbsent(body)));
    }
    // Qshell's db2 utility (qshdb2.rs), at its Qshell path.
    v.push((
        "/usr/bin/db2".into(),
        PaseFile::IfAbsent(format!("#!/bin/sh\nexec {exe} db2 \"$@\"\n")),
    ));
    v.push(("/QOpenSys/usr/bin/qsh".into(), PaseFile::Script(QSH.into())));
    for sh in ["sh", "bsh", "ksh"] {
        v.push((
            format!("/QOpenSys/usr/bin/{sh}"),
            PaseFile::Link("/bin/sh".into()),
        ));
    }
    const TOOLS: &[&str] = &[
        "awk",
        "basename",
        "cat",
        "chmod",
        "chown",
        "cp",
        "cut",
        "date",
        "diff",
        "dirname",
        "echo",
        "env",
        "expr",
        "find",
        "getconf",
        "grep",
        "head",
        "id",
        "ln",
        "ls",
        "md5sum",
        "mkdir",
        "mv",
        "openssl",
        "pwd",
        "rm",
        "rmdir",
        "sed",
        "sha256sum",
        "sort",
        "stat",
        "tail",
        "tar",
        "tee",
        "test",
        "touch",
        "tr",
        "uniq",
        "wc",
        "xargs",
    ];
    for t in TOOLS {
        if let Some(p) = container_program(t) {
            v.push((format!("/QOpenSys/usr/bin/{t}"), PaseFile::Link(p)));
        }
    }
    const PKGS: &[&str] = &[
        "bash",
        "git",
        "grep",
        "md5sum",
        "sha256sum",
        "stat",
        "sort",
        "tar",
        "ls",
        "find",
    ];
    for t in PKGS {
        if let Some(p) = container_program(t) {
            v.push((format!("/QOpenSys/pkgs/bin/{t}"), PaseFile::Link(p)));
        }
    }
    // iconv: PASE's names its code sets IBM-NNN (IBM-273, IBM-1208, as
    // Code for IBM i passes them: IBMiContent.ts convertFromUTF8); the
    // container's iconv knows them as IBM273, IBM037, UTF-8.
    v.push((
        "/QOpenSys/usr/bin/iconv".into(),
        PaseFile::Script(ICONV.to_string()),
    ));
    // Where the container's iconv is bound in a command's namespace.
    v.push((
        "/QOpenSys/usr/lib/iconv-real".into(),
        PaseFile::IfAbsent(String::new()),
    ));
    // Java: the Mapepire server it is asked to run is as4tainr's.
    for jdk in ["jdk80", "jdk11", "jdk17", "jdk21"] {
        v.push((
            format!("/QOpenSys/QIBM/ProdData/JavaVM/{jdk}/64bit/bin/java"),
            PaseFile::Script(format!("#!/bin/sh\nexec {exe} mapepire \"$@\"\n")),
        ));
    }
    v
}

/// Qshell (qsh), as as4tainr gives it: the container's shell, with
/// Qshell's `liblist` built in (Qshell: `liblist [-acdfl] [library ...]`
/// adds libraries to the user part, at the front unless -l; -c sets the
/// current library; -d takes libraries off; no option shows the list).
/// The list is AS4TAINR_LIBL and AS4TAINR_CURLIB, which `system` uses.
/// The command comes from -c or, without it, standard input.
const QSH: &str = r#"#!/bin/sh
liblist() {
  _op=""; _end=f
  while [ $# -gt 0 ]; do
    case "$1" in
      -a) _op=a;; -c) _op=c;; -d) _op=d;; -f) _end=f;; -l) _end=l;;
      -*) ;;
      *) break;;
    esac
    shift
  done
  [ -z "$_op" ] && [ $# -gt 0 ] && _op=a
  _l="${AS4TAINR_LIBL-QGPL,QTEMP}"
  _rm() {
    _o=""; _s=$IFS; IFS=,
    for _y in $_l; do [ -n "$_y" ] && [ "$_y" != "$1" ] && _o="$_o,$_y"; done
    IFS=$_s; _l="${_o#,}"
  }
  case "$_op" in
    c) AS4TAINR_CURLIB=$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]');;
    a|d)
      for _x in "$@"; do
        _x=$(printf '%s' "$_x" | tr '[:lower:]' '[:upper:]')
        _rm "$_x"
        if [ "$_op" = a ]; then
          if [ "$_end" = l ]; then _l="${_l:+$_l,}$_x"; else _l="$_x${_l:+,$_l}"; fi
        fi
      done;;
    *)
      for _y in QSYS QSYS2 QHLPSYS QUSRSYS; do printf '%-10s SYSTEM
' "$_y"; done
      [ -n "$AS4TAINR_CURLIB" ] && printf '%-10s CURRENT
' "$AS4TAINR_CURLIB"
      _s=$IFS; IFS=,; for _y in $_l; do printf '%-10s USER
' "$_y"; done; IFS=$_s;;
  esac
  AS4TAINR_LIBL="$_l"; export AS4TAINR_LIBL AS4TAINR_CURLIB
}
# Started through a link with a utility's name (ln -s .../qsh grep), qsh
# runs that Qshell utility (Code for IBM i's member search,
# SearchTools.ts): here the container's.
_me="${0##*/}"
case "$_me" in
  qsh|qsh_inout|qsh_out|sh) ;;
  *) for _d in /usr/bin /bin; do [ -x "$_d/$_me" ] && exec "$_d/$_me" "$@"; done
     echo "qsh: $_me: not found" >&2; exit 127;;
esac
if [ "$1" = "-c" ]; then
  shift
  eval "$1"
  exit $?
fi
if [ $# -gt 0 ]; then
  . "$@"
  exit $?
fi
eval "$(cat)"
"#;

const ICONV: &str = r#"#!/bin/sh
# PASE's iconv code set names (IBM-NNN) as the container's iconv has them.
name() {
  case "$1" in
    IBM-1208|ibm-1208|UTF8|utf8) echo UTF-8;;
    IBM-819|ibm-819|ISO8859-1|iso8859-1) echo ISO-8859-1;;
    IBM-1200|ibm-1200|UCS-2|ucs-2) echo UTF-16BE;;
    IBM-*|ibm-*) n=${1#*-}
      case "$n" in ??) n=0$n;; ?) n=00$n;; esac
      echo IBM$n;;
    *) echo "$1";;
  esac
}
first=1; conv=""
for a; do
  if [ -n "$first" ]; then set --; first=""; fi
  if [ -n "$conv" ]; then set -- "$@" "$(name "$a")"; conv=""; continue; fi
  case "$a" in -f|-t) conv=1;; esac
  set -- "$@" "$a"
done
# In a command's namespace /usr/bin/iconv is this script (qshfs.rs) and
# the container's iconv is bound at iconv-real.
real=/QOpenSys/usr/lib/iconv-real
[ -s "$real" ] || real=/usr/bin/iconv
exec "$real" "$@"
"#;

const UNAME: &str = r#"#!/bin/sh
# PASE's uname, as as4tainr answers it.
s=OS400; n=$(/bin/hostname 2>/dev/null || echo AS4TAINR); r=5; v=7; m=00000000AS4T
[ $# -eq 0 ] && { echo "$s"; exit 0; }
out=""
add() { out="${out:+$out }$1"; }
for a in "$@"; do
  case "$a" in
    -a) add "$s"; add "$n"; add "$r"; add "$v"; add "$m";;
    -*) f=${a#-}
        while [ -n "$f" ]; do
          c=${f%"${f#?}"}; f=${f#?}
          case "$c" in s) add "$s";; n) add "$n";; r) add "$r";; v) add "$v";; m|M) add "$m";; p) add powerpc;;
            *) echo "uname: not a valid option -- $c" >&2; exit 1;; esac
        done;;
    *) echo "usage: uname [-amnprsv]" >&2; exit 1;;
  esac
done
echo "$out"
"#;

enum PaseFile {
    Script(String),
    /// A script made only where there is no file of that name, or there
    /// is as4tainr's own.
    IfAbsent(String),
    Link(String),
}

fn container_program(name: &str) -> Option<String> {
    ["/usr/bin", "/bin", "/usr/local/bin", "/usr/sbin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
}

/// Make the PASE programs (those already there are left alone, unless
/// they are as4tainr's scripts of an earlier version).
pub fn install_pase_tree() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for (path, f) in pase_files() {
        let p = std::path::Path::new(&path);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        match f {
            PaseFile::Script(body) => {
                if std::fs::read_to_string(p).ok().as_deref() != Some(body.as_str()) {
                    let _ = std::fs::remove_file(p);
                    std::fs::write(p, body)?;
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            PaseFile::IfAbsent(body) => {
                let ours = std::fs::symlink_metadata(p).is_err()
                    || std::fs::read(p)
                        .map(|b| String::from_utf8_lossy(&b).contains("AS4TAINR_EXE"))
                        .unwrap_or(false);
                if ours && std::fs::read_to_string(p).ok().as_deref() != Some(body.as_str()) {
                    let _ = std::fs::remove_file(p);
                    std::fs::write(p, body)?;
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            PaseFile::Link(to) => {
                if std::fs::symlink_metadata(p).is_err() {
                    std::os::unix::fs::symlink(to, p)?;
                }
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------- sessions

#[derive(Clone)]
struct Server {
    database_url: Arc<String>,
}

impl russh::server::Server for Server {
    type Handler = Connection;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Connection {
        Connection {
            database_url: self.database_url.clone(),
            user: None,
            channels: HashMap::new(),
            env: HashMap::new(),
            ptys: Default::default(),
        }
    }
}

struct Connection {
    database_url: Arc<String>,
    /// The signed-on user profile.
    user: Option<String>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// Environment variables the client set for a channel.
    env: HashMap<ChannelId, Vec<(String, String)>>,
    /// Channels a terminal (pty) was asked for.
    ptys: std::collections::HashSet<ChannelId>,
}

impl russh::server::Handler for Connection {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth> {
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
        let client = crate::runtime::connect_db(&self.database_url).await?;
        match crate::users::authenticate(&client, user, password).await {
            Ok(p) if !p.pwd_expired => {
                self.user = Some(p.name);
                Ok(Auth::Accept)
            }
            _ => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<()> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<()> {
        self.env
            .entry(channel)
            .or_default()
            .push((name.to_string(), value.to_string()));
        session.channel_success(channel)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<()> {
        self.ptys.insert(channel);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        let (Some(ch), Some(user)) = (self.channels.remove(&channel), self.user.clone()) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let line = String::from_utf8_lossy(data).to_string();
        let env = self.env.remove(&channel).unwrap_or_default();
        let url = self.database_url.clone();
        tokio::spawn(async move {
            if let Err(e) = exec(&url, &user, ch, &line, env).await {
                eprintln!("as4tainr: SSH command of {user} ended: {e:#}");
            }
        });
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
        let (Some(ch), Some(user)) = (self.channels.remove(&channel), self.user.clone()) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let env = self.env.remove(&channel).unwrap_or_default();
        let terminal = self.ptys.remove(&channel);
        let url = self.database_url.clone();
        tokio::spawn(async move {
            if let Err(e) = interactive(&url, &user, ch, env, terminal).await {
                eprintln!("as4tainr: SSH session of {user} ended: {e:#}");
            }
        });
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<()> {
        let user = self.user.clone();
        match (name, self.channels.remove(&channel), user) {
            ("sftp", Some(ch), Some(user)) => {
                let client = crate::runtime::connect_db(&self.database_url).await?;
                let all = crate::users::get(&client, &user)
                    .await?
                    .is_some_and(|p| p.special_auth.iter().any(|a| a == "*ALLOBJ"));
                let home = home_dir(&client, &user).await;
                session.channel_success(channel)?;
                let sftp = Sftp {
                    client: Arc::new(client),
                    user,
                    all,
                    home,
                    handles: HashMap::new(),
                    next: 0,
                };
                russh_sftp::server::run(ch.into_stream(), sftp).await;
            }
            _ => session.channel_failure(channel)?,
        }
        Ok(())
    }
}

/// The user's home directory in the IFS (/home/USER), or / when there is
/// none.
async fn home_dir(client: &Client, user: &str) -> String {
    let home = format!("/home/{user}");
    match crate::ifs::stat(client, &home).await {
        Ok(Some(e)) if e.dir => e.path,
        _ => "/".into(),
    }
}

/// Is this command line the Mapepire server's start (`[cd DIR &&] exec
/// env NAME=VALUE... JAVA [options] -jar JAR ... --single`)?
pub fn is_mapepire_start(line: &str) -> bool {
    let words = shell_words(line);
    let mut w = words.iter().map(String::as_str).peekable();
    // cd DIR &&
    if w.peek() == Some(&"cd") {
        w.next();
        w.next();
        if w.next() != Some("&&") {
            return false;
        }
    }
    if w.peek() == Some(&"exec") {
        w.next();
    }
    if w.peek() == Some(&"env") {
        w.next();
    }
    while w
        .peek()
        .is_some_and(|x| x.contains('=') && !x.starts_with('-'))
    {
        w.next();
    }
    let Some(java) = w.next() else {
        return false;
    };
    let rest: Vec<&str> = w.collect();
    (java == "java" || java.ends_with("/java"))
        && rest.contains(&"-jar")
        && rest.contains(&"--single")
        && !rest.iter().any(|x| matches!(*x, ";" | "|" | "&&" | "||"))
}

/// A command line's words (quotes and backslashes taken off; `&&`, `||`,
/// `;` and `|` as words of their own).
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                have = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    cur.push(d);
                }
            }
            '"' => {
                have = true;
                while let Some(d) = chars.next() {
                    match d {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                cur.push(e);
                            }
                        }
                        _ => cur.push(d),
                    }
                }
            }
            '\\' => {
                have = true;
                if let Some(e) = chars.next() {
                    cur.push(e);
                }
            }
            ';' | '|' | '&' => {
                if have {
                    words.push(std::mem::take(&mut cur));
                    have = false;
                }
                let mut op = c.to_string();
                if chars.peek() == Some(&c) && c != ';' {
                    chars.next();
                    op.push(c);
                }
                words.push(op);
            }
            c if c.is_whitespace() => {
                if have {
                    words.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                have = true;
                cur.push(c);
            }
        }
    }
    if have {
        words.push(cur);
    }
    words
}

/// A command word replaced (`/usr/bin/uname` by PASE's): only where it
/// stands as a word.
fn replace_word(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(from) {
            let before_ok = out
                .chars()
                .last()
                .is_none_or(|c| c.is_whitespace() || "'\";|&(`".contains(c));
            let after_ok = after
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || "'\";|&)`".contains(c));
            if before_ok && after_ok {
                out.push_str(to);
                rest = after;
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Run one command for the user on the channel: its output, its error
/// output and its exit status go back.
async fn exec(
    url: &str,
    user: &str,
    ch: Channel<Msg>,
    line: &str,
    vars: Vec<(String, String)>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let client = crate::runtime::connect_db(url).await?;
    let (mut rd, wr) = ch.split();
    let code: u32 = if is_mapepire_start(line) {
        let env = crate::commands::Environment {
            database_url: url.to_string(),
        };
        let libl = crate::commands::USER_LIBRARY_LIST
            .iter()
            .map(|l| l.to_string())
            .collect();
        let legacy = crate::mapepire::legacy_cl_for(&shell_words(line));
        let r = crate::mapepire::serve(
            &client,
            &env,
            user,
            libl,
            legacy,
            rd.make_reader(),
            wr.make_writer(),
        )
        .await;
        match r {
            Ok(()) => 0,
            Err(e) => {
                let mut err = wr.make_writer_ext(Some(1));
                let _ = err.write_all(format!("{e:#}\n").as_bytes()).await;
                1
            }
        }
    } else {
        let home = home_dir(&client, user).await;
        let (mut p, mirror, notes) =
            shell_command(&client, url, user, &home, &home, line, &vars).await?;
        p.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let code = match p.spawn() {
            Ok(mut child) => {
                let mut stdin = child.stdin.take();
                let mut stdout = child.stdout.take().expect("stdout");
                let mut stderr = child.stderr.take().expect("stderr");
                let mut out = wr.make_writer();
                let mut err = wr.make_writer_ext(Some(1));
                let input = async {
                    let mut reader = rd.make_reader();
                    if let Some(s) = stdin.as_mut() {
                        let _ = tokio::io::copy(&mut reader, s).await;
                    }
                    drop(stdin.take());
                };
                let output = async {
                    let _ = tokio::join!(
                        tokio::io::copy(&mut stdout, &mut out),
                        tokio::io::copy(&mut stderr, &mut err)
                    );
                    let _ = out.flush().await;
                    let _ = err.flush().await;
                    child.wait().await
                };
                tokio::pin!(input);
                tokio::pin!(output);
                let mut input_done = false;
                let status = loop {
                    tokio::select! {
                        s = &mut output => break s,
                        _ = &mut input, if !input_done => input_done = true,
                    }
                };
                match status {
                    Ok(s) => s
                        .code()
                        .map(|c| c as u32)
                        .unwrap_or_else(|| 128 + signal_of(&s)),
                    Err(_) => 127,
                }
            }
            Err(e) => {
                let mut err = wr.make_writer_ext(Some(1));
                let _ = err
                    .write_all(format!("as4tainr: the command did not start: {e}\n").as_bytes())
                    .await;
                127
            }
        };
        if let Some(m) = mirror {
            m.close(&client, user).await?;
        }
        crate::pase_attr::apply_notes(&client, &notes).await;
        code
    };
    wr.exit_status(code).await?;
    wr.eof().await?;
    wr.close().await?;
    Ok(())
}

/// A shell command line as an SSH session runs it: on the IFS mirror with
/// /QSYS.LIB (qshfs.rs, qsyslib.rs), starting in `cwd`, with the
/// session's environment; the mirror to close after it, and the file of
/// CCSIDs `attr` and `setccsid` set (pase_attr.rs).
async fn shell_command(
    client: &tokio_postgres::Client,
    url: &str,
    user: &str,
    home: &str,
    cwd: &str,
    line: &str,
    vars: &[(String, String)],
) -> Result<(
    tokio::process::Command,
    Option<crate::qshfs::Mirror>,
    std::path::PathBuf,
)> {
    let line = replace_word(line, "/usr/bin/uname", "/QOpenSys/usr/bin/uname");
    let line = crate::qsyslib::upper_paths(&line);
    let cwd = &crate::qsyslib::upper_paths(cwd);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let notes = std::path::PathBuf::from(format!(
        "/var/tmp/as4tainr-attr-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mirror = if crate::qshfs::available() {
        let mut m = crate::qshfs::Mirror::open(client, url).await?;
        m.prepare_qsys(client, cwd, &line).await?;
        Some(m)
    } else {
        None
    };
    let mut p = match mirror
        .as_ref()
        .and_then(|m| m.command_with(cwd, &line, SHELL, false))
    {
        Some((prog, args)) => {
            let mut c = tokio::process::Command::new(prog);
            c.args(args);
            c
        }
        None => {
            let mut c = tokio::process::Command::new(SHELL);
            c.arg("-c").arg(&line).current_dir(std::env::temp_dir());
            c
        }
    };
    p.env_clear()
        .env(
            "PATH",
            "/QOpenSys/pkgs/bin:/QOpenSys/usr/bin:/usr/local/bin:/usr/bin:/bin",
        )
        .env("HOME", home)
        .env("USER", user)
        .env("LOGNAME", user)
        .env("SHELL", SHELL)
        .env("LANG", "C.UTF-8")
        .env("AS4TAINR_EXE", format!("/proc/{}/exe", std::process::id()))
        .env("AS4TAINR_DATABASE_URL", url)
        .env("AS4TAINR_USER", user)
        .env("AS4TAINR_ATTR_NOTES", &notes);
    for (k, v) in vars {
        if !k.starts_with("AS4TAINR_") && k != "PATH" {
            p.env(k, v);
        }
    }
    Ok((p, mirror, notes))
}

/// An interactive session (`ssh host` with no command): each line typed
/// is run as a command (as `exec` runs one), in the directory the one
/// before ended in, so `cd` carries over; `exit` ends it. With a
/// terminal (a pty was asked for) the session echoes what is typed and
/// shows a prompt, as a shell does; commands do not read the terminal
/// (their standard input is empty) and their output comes when they end.
async fn interactive(
    url: &str,
    user: &str,
    ch: Channel<Msg>,
    vars: Vec<(String, String)>,
    terminal: bool,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let client = crate::runtime::connect_db(url).await?;
    let (mut rd, wr) = ch.split();
    let mut input = rd.make_reader();
    let mut out = wr.make_writer();
    let home = home_dir(&client, user).await;
    let mut cwd = home.clone();
    let nl = |b: &[u8]| -> Vec<u8> {
        if !terminal {
            return b.to_vec();
        }
        let mut v = Vec::with_capacity(b.len());
        for &c in b {
            if c == b'\n' {
                v.push(b'\r');
            }
            v.push(c);
        }
        v
    };
    let prompt = |cwd: &str| format!("{}$ ", if cwd == home { "~" } else { cwd });
    if terminal {
        out.write_all(prompt(&cwd).as_bytes()).await?;
        out.flush().await?;
    }
    let mut line: Vec<u8> = Vec::new();
    let mut buf = [0u8; 1024];
    let mut last = 0u32;
    let mut prev_cr = false;
    'session: loop {
        let n = input.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        for &c in &buf[..n] {
            match c {
                // The \n of a \r\n ends nothing more.
                b'\n' if prev_cr => prev_cr = false,
                b'\r' | b'\n' => {
                    prev_cr = c == b'\r';
                    if terminal {
                        out.write_all(b"\r\n").await?;
                    }
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    line.clear();
                    if text == "exit" || text.starts_with("exit ") {
                        last = text[4..].trim().parse().unwrap_or(last);
                        break 'session;
                    }
                    if !text.is_empty() {
                        let marker = format!(
                            "/var/tmp/as4tainr-cwd-{}-{}",
                            std::process::id(),
                            rand::random::<u32>()
                        );
                        let wrapped = format!(
                            "{text}\n__as4tainr_rc=$?; pwd > {marker} 2>/dev/null; exit $__as4tainr_rc"
                        );
                        let (mut p, mirror, notes) =
                            shell_command(&client, url, user, &home, &cwd, &wrapped, &vars).await?;
                        p.stdin(std::process::Stdio::null());
                        let o = p.output().await;
                        if let Some(m) = mirror {
                            m.close(&client, user).await?;
                        }
                        crate::pase_attr::apply_notes(&client, &notes).await;
                        match o {
                            Ok(o) => {
                                out.write_all(&nl(&o.stdout)).await?;
                                let mut err = wr.make_writer_ext(Some(1));
                                err.write_all(&nl(&o.stderr)).await?;
                                err.flush().await?;
                                last = o
                                    .status
                                    .code()
                                    .map(|c| c as u32)
                                    .unwrap_or_else(|| 128 + signal_of(&o.status));
                            }
                            Err(e) => {
                                out.write_all(nl(format!("as4tainr: {e}\n").as_bytes()).as_slice())
                                    .await?;
                                last = 127;
                            }
                        }
                        if let Ok(d) = std::fs::read_to_string(&marker) {
                            let d = d.trim();
                            if !d.is_empty() {
                                cwd = d.to_string();
                            }
                        }
                        let _ = std::fs::remove_file(&marker);
                    }
                    if terminal {
                        out.write_all(prompt(&cwd).as_bytes()).await?;
                    }
                    out.flush().await?;
                }
                // Backspace and delete.
                0x7f | 0x08 => {
                    prev_cr = false;
                    if line.pop().is_some() && terminal {
                        out.write_all(b"\x08 \x08").await?;
                        out.flush().await?;
                    }
                }
                // Ctrl-C: the line is dropped.
                0x03 => {
                    line.clear();
                    if terminal {
                        out.write_all(b"^C\r\n").await?;
                        out.write_all(prompt(&cwd).as_bytes()).await?;
                        out.flush().await?;
                    }
                }
                // Ctrl-D on an empty line ends the session.
                0x04 if line.is_empty() => break 'session,
                c if c >= 0x20 || c == b'\t' => {
                    prev_cr = false;
                    line.push(c);
                    if terminal {
                        out.write_all(&[c]).await?;
                        out.flush().await?;
                    }
                }
                _ => {}
            }
        }
    }
    if terminal {
        let _ = out.write_all(b"\r\n").await;
    }
    let _ = out.flush().await;
    drop(out);
    wr.exit_status(last).await?;
    wr.eof().await?;
    wr.close().await?;
    Ok(())
}

fn signal_of(s: &std::process::ExitStatus) -> u32 {
    use std::os::unix::process::ExitStatusExt;
    s.signal().unwrap_or(0) as u32
}

// ------------------------------------------------------------------ SFTP

/// An open file (its bytes, read or written whole) or directory (its
/// entries, listed once).
enum Open {
    File {
        path: String,
        data: Vec<u8>,
        write: bool,
        changed: bool,
        ccsid: i32,
    },
    Dir {
        entries: Vec<File>,
        sent: bool,
    },
}

struct Sftp {
    client: Arc<Client>,
    user: String,
    /// *ALLOBJ: every permission.
    all: bool,
    home: String,
    handles: HashMap<String, Open>,
    next: u64,
}

fn status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

/// An IFS error as an SFTP status.
fn reply(e: anyhow::Error) -> StatusReply {
    use crate::ifs::errno;
    let code = match crate::ifs::errno_of(&e) {
        errno::ENOENT => StatusCode::NoSuchFile,
        errno::EACCES | errno::EPERM => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    };
    StatusReply::new(code).with_message(format!("{e:#}"))
}

fn attrs_of(e: &crate::ifs::Entry) -> FileAttributes {
    let mut a = FileAttributes {
        size: Some(e.size.max(0) as u64),
        uid: Some(0),
        user: Some(e.owner.to_ascii_lowercase()),
        gid: Some(0),
        group: Some("0".into()),
        permissions: Some((e.mode & 0o7777) as u32),
        atime: Some((e.changed_us / 1_000_000) as u32),
        mtime: Some((e.changed_us / 1_000_000) as u32),
    };
    if e.dir {
        a.set_dir(true);
    } else {
        a.set_regular(true);
    }
    a
}

impl Sftp {
    /// A path the client gave, absolute (relative ones from the home
    /// directory).
    fn abs(&self, p: &str) -> String {
        let p = p.trim();
        let full = if p.is_empty() || p == "." {
            self.home.clone()
        } else if p.starts_with('/') {
            p.to_string()
        } else {
            format!("{}/{p}", self.home.trim_end_matches('/'))
        };
        crate::ifs::normalize(&full).unwrap_or(full)
    }

    /// May the user read (4), write (2) or search (1) this object?
    fn may(&self, e: &crate::ifs::Entry, bit: i32) -> bool {
        if self.all {
            return true;
        }
        let bits = if e.owner.eq_ignore_ascii_case(&self.user) {
            (e.mode >> 6) & 7
        } else {
            e.mode & 7
        };
        bits & bit != 0
    }

    async fn entry(&self, path: &str) -> Result<crate::ifs::Entry, StatusReply> {
        match crate::ifs::stat(&self.client, path).await {
            Ok(Some(e)) => Ok(e),
            Ok(None) => Err(StatusCode::NoSuchFile.into()),
            Err(e) => Err(reply(e)),
        }
    }

    /// The directory a new object goes in: it must exist and be writable.
    async fn writable_parent(&self, path: &str) -> Result<(), StatusReply> {
        let parent = match path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(i) => path[..i].to_string(),
        };
        let d = self.entry(&parent).await?;
        if !self.may(&d, 2) {
            return Err(StatusCode::PermissionDenied.into());
        }
        Ok(())
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let h = format!("h{}", self.next);
        self.handles.insert(h.clone(), open);
        h
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusReply;

    fn unimplemented(&self) -> StatusReply {
        StatusCode::OpUnsupported.into()
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, StatusReply> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, StatusReply> {
        let path = self.abs(&filename);
        let write = pflags.intersects(
            OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        );
        let found = crate::ifs::stat(&self.client, &path).await.map_err(reply)?;
        let (data, ccsid, changed) = match found {
            Some(e) if e.dir => return Err(StatusCode::Failure.with_message("Is a directory")),
            Some(_)
                if pflags.contains(OpenFlags::EXCLUDE) && pflags.contains(OpenFlags::CREATE) =>
            {
                return Err(StatusCode::Failure.with_message("File exists"));
            }
            Some(e) => {
                if (pflags.contains(OpenFlags::READ) && !self.may(&e, 4))
                    || (write && !self.may(&e, 2))
                {
                    return Err(StatusCode::PermissionDenied.into());
                }
                if pflags.contains(OpenFlags::TRUNCATE) {
                    (vec![], e.ccsid, true)
                } else {
                    let (d, c) = crate::ifs::read_file(&self.client, &path)
                        .await
                        .map_err(reply)?;
                    (d, c, false)
                }
            }
            None if pflags.contains(OpenFlags::CREATE) => {
                self.writable_parent(&path).await?;
                let mode = attrs
                    .permissions
                    .map(|p| (p & 0o7777) as i32)
                    .unwrap_or(0o644);
                // A new file is made at once (another client sees it), as
                // open(O_CREAT) makes it; ASCII files are tagged 819.
                crate::ifs::write_file(&self.client, &path, &[], 819, mode, &self.user)
                    .await
                    .map_err(reply)?;
                (vec![], 819, false)
            }
            None => return Err(StatusCode::NoSuchFile.into()),
        };
        let h = self.handle(Open::File {
            path,
            data,
            write,
            changed,
            ccsid,
        });
        Ok(Handle { id, handle: h })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusReply> {
        if let Some(Open::File {
            path,
            data,
            write: true,
            changed: true,
            ccsid,
        }) = self.handles.remove(&handle)
        {
            crate::ifs::write_file(&self.client, &path, &data, ccsid, 0o644, &self.user)
                .await
                .map_err(reply)?;
        }
        Ok(status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusReply> {
        match self.handles.get(&handle) {
            Some(Open::File { data, .. }) => {
                let start = offset as usize;
                if start >= data.len() {
                    return Err(StatusCode::Eof.into());
                }
                let end = (start + len as usize).min(data.len());
                Ok(Data {
                    id,
                    data: data[start..end].to_vec(),
                })
            }
            _ => Err(StatusCode::Failure.with_message("not an open file")),
        }
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Status, StatusReply> {
        match self.handles.get_mut(&handle) {
            Some(Open::File {
                data,
                write: true,
                changed,
                ..
            }) => {
                let start = offset as usize;
                let end = start + bytes.len();
                if data.len() < end {
                    data.resize(end, 0);
                }
                data[start..end].copy_from_slice(&bytes);
                *changed = true;
                Ok(status(id))
            }
            _ => Err(StatusCode::PermissionDenied.into()),
        }
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        self.stat(id, path).await
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        let p = self.abs(&path);
        let e = self.entry(&p).await?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&e),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusReply> {
        let (path, size) = match self.handles.get(&handle) {
            Some(Open::File { path, data, .. }) => (path.clone(), data.len() as i64),
            _ => return Err(StatusCode::Failure.with_message("not an open file")),
        };
        let mut e = self.entry(&path).await?;
        e.size = size;
        Ok(Attrs {
            id,
            attrs: attrs_of(&e),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let p = self.abs(&path);
        let e = self.entry(&p).await?;
        if let Some(perm) = attrs.permissions {
            if !self.all && !e.owner.eq_ignore_ascii_case(&self.user) {
                return Err(StatusCode::PermissionDenied.into());
            }
            crate::ifs::set_mode(&self.client, &p, (perm & 0o7777) as i32)
                .await
                .map_err(reply)?;
        }
        Ok(status(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let path = match self.handles.get(&handle) {
            Some(Open::File { path, .. }) => path.clone(),
            _ => return Ok(status(id)),
        };
        self.setstat(id, path, attrs).await
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusReply> {
        let p = self.abs(&path);
        let d = self.entry(&p).await?;
        if !d.dir {
            return Err(StatusCode::Failure.with_message("Not a directory"));
        }
        if !self.may(&d, 4) {
            return Err(StatusCode::PermissionDenied.into());
        }
        let names = crate::ifs::list(&self.client, &p).await.map_err(reply)?;
        let mut entries = vec![File::new(".", attrs_of(&d)), File::new("..", attrs_of(&d))];
        for (name, _) in names {
            let full = if p == "/" {
                format!("/{name}")
            } else {
                format!("{p}/{name}")
            };
            if let Ok(Some(e)) = crate::ifs::stat(&self.client, &full).await {
                entries.push(File::new(name, attrs_of(&e)));
            }
        }
        let h = self.handle(Open::Dir {
            entries,
            sent: false,
        });
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusReply> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir { entries, sent }) => {
                if *sent {
                    return Err(StatusCode::Eof.into());
                }
                *sent = true;
                Ok(Name {
                    id,
                    files: std::mem::take(entries),
                })
            }
            _ => Err(StatusCode::Failure.with_message("not an open directory")),
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusReply> {
        let p = self.abs(&filename);
        self.entry(&p).await?;
        self.writable_parent(&p).await?;
        crate::ifs::unlink(&self.client, &p).await.map_err(reply)?;
        Ok(status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let p = self.abs(&path);
        self.writable_parent(&p).await?;
        let mode = attrs
            .permissions
            .map(|p| (p & 0o7777) as i32)
            .unwrap_or(0o755);
        crate::ifs::mkdir(&self.client, &p, mode, &self.user)
            .await
            .map_err(reply)?;
        Ok(status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusReply> {
        let p = self.abs(&path);
        self.entry(&p).await?;
        self.writable_parent(&p).await?;
        crate::ifs::rmdir(&self.client, &p).await.map_err(reply)?;
        Ok(status(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusReply> {
        Ok(Name {
            id,
            files: vec![File::dummy(self.abs(&path))],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusReply> {
        let (o, n) = (self.abs(&oldpath), self.abs(&newpath));
        self.entry(&o).await?;
        self.writable_parent(&o).await?;
        self.writable_parent(&n).await?;
        crate::ifs::rename(&self.client, &o, &n)
            .await
            .map_err(reply)?;
        Ok(status(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapepire_start_is_recognized() {
        // As mapepire-js 1.0.1 builds it (ssh-single transport).
        assert!(is_mapepire_start(
            "exec env QIBM_JAVA_STDIO_CONVERT='N' QIBM_PASE_DESCRIPTOR_STDIO='B' QIBM_USE_DESCRIPTOR_STDIO='Y' QIBM_MULTI_THREADED='Y' '/QOpenSys/QIBM/ProdData/JavaVM/jdk80/64bit/bin/java' '-Djdbc.db2.restricted.local.connection.only=true' '-Dos400.stdio.convert=N' '-jar' '/home/ME/.mapepire/mapepire-server-2.3.5.jar' '--single'"
        ));
        assert!(is_mapepire_start(
            "cd '/home/ME' && exec env A='1' java -jar x.jar --single"
        ));
        assert!(!is_mapepire_start("java -jar x.jar"));
        assert!(!is_mapepire_start("java -jar x.jar --single; rm -rf /"));
        assert!(!is_mapepire_start("ls -l"));
    }

    #[test]
    fn uname_path_replaced_as_a_word() {
        assert_eq!(
            replace_word(
                "/usr/bin/uname -rv",
                "/usr/bin/uname",
                "/QOpenSys/usr/bin/uname"
            ),
            "/QOpenSys/usr/bin/uname -rv"
        );
        assert_eq!(
            replace_word("ls /usr/bin/unamex", "/usr/bin/uname", "X"),
            "ls /usr/bin/unamex"
        );
    }
}
