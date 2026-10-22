use anyhow::{bail, Context, Result};
use as4tainr_core::{catalog, commands, dds, runtime, tn5250, users};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "as4tainr",
    version,
    about = "as4tainr: an IBM i-compatible runtime"
)]
struct Cli {
    /// PostgreSQL connection URL.
    #[arg(
        long,
        global = true,
        env = "AS4TAINR_DATABASE_URL",
        default_value = "postgres://as4tainr:as4tainr@127.0.0.1/as4tainr"
    )]
    database_url: String,

    /// User part of the library list, comma-separated, searched in order
    /// (after the system libraries and the current library in a 5250 job).
    #[arg(
        long,
        global = true,
        env = "AS4TAINR_LIBL",
        default_value = "QGPL,QTEMP",
        value_delimiter = ','
    )]
    libl: Vec<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the catalog and the system libraries (idempotent).
    Init,
    /// Reset a user's password to the user name, expired, so the next
    /// sign-on must change it. For a forgotten QSECOFR password.
    Rstpwd {
        /// User profile (default QSECOFR).
        #[arg(default_value = "QSECOFR")]
        user: String,
    },
    /// CRTLIB: create a library.
    Crtlib {
        library: String,
        #[arg(long, default_value = "")]
        text: String,
    },
    /// CRTPF: create a physical file from DDS source.
    Crtpf {
        /// Qualified name, LIB/FILE.
        file: String,
        /// DDS source file.
        #[arg(long)]
        src: PathBuf,
    },
    /// CRTLF: create a logical file over a physical file from DDS source.
    /// The physical file named in PFILE is found through the library list
    /// unless it is qualified.
    Crtlf {
        /// Qualified name, LIB/FILE.
        file: String,
        /// DDS source file.
        #[arg(long)]
        src: PathBuf,
    },
    /// CRTBNDCL: check a CL program and create it (replacing one of the
    /// same name, as REPLACE(*YES) does).
    Crtbndcl {
        /// Qualified name, LIB/PGM.
        program: String,
        /// CL source file.
        #[arg(long)]
        src: PathBuf,
        /// Text description.
        #[arg(long, default_value = "")]
        text: String,
    },
    /// Run one SQL statement (DB2 for i dialect) with the library list.
    Sql {
        /// The statement, e.g. "SELECT * FROM CUSTMAST".
        statement: String,
    },
    /// CRTDSPF: create a display file from DDS source.
    Crtdspf {
        /// Qualified name, LIB/FILE.
        file: String,
        /// DDS source file.
        #[arg(long)]
        src: PathBuf,
    },
    /// Import an application: source members (folders like QRPGLESRC/ or
    /// TOBi project folders, files like ORDENT.RPGLE or
    /// ORDENT-Order_entry.pgm.rpgle) and CSV data into a library, then
    /// build its files and programs with the create commands.
    Import {
        /// The directory (a clone of the application's git repository).
        dir: PathBuf,
        /// The library to import into (created if needed).
        #[arg(long)]
        lib: String,
    },
    /// Copy a file, or a folder and everything in it, into the integrated
    /// file system (as copying it to an IBM i would), tagged with a CCSID.
    Put {
        /// The local file or folder.
        from: PathBuf,
        /// The IFS path it becomes (directories above it are made).
        to: String,
        /// The CCSID the stream files are tagged with.
        #[arg(long, default_value_t = 1208)]
        ccsid: i32,
    },
    /// Copy a stream file out of the integrated file system to a local
    /// file, its bytes as they are (e.g. a save file copied with CPYTOSTMF,
    /// to restore on another system).
    Get {
        /// The IFS path.
        from: String,
        /// The local file it becomes.
        to: PathBuf,
    },
    /// Load records from a CSV file (header row = field names) with WRITE.
    Load {
        /// File name, optionally qualified LIB/FILE.
        file: String,
        #[arg(long)]
        csv: PathBuf,
    },
    /// Run one CL command as QSECOFR, as the IBM i PASE `system` utility
    /// does (e.g. `as4tainr system "CRTDTAARA DTAARA(QGPL/X) TYPE(*DEC)"`):
    /// messages and printed output on standard output, a failure's
    /// messages on standard error with exit status 255. Options -n (no
    /// message IDs), -v (show the command), -s (no printed output).
    #[command(disable_help_flag = true)]
    System {
        /// Options, then the command as typed on a command line.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the TN5250 display server (5250 green-screen sessions).
    Tn5250 {
        /// Address to listen on.
        #[arg(long, env = "AS4TAINR_TN5250_LISTEN", default_value = "0.0.0.0:23")]
        listen: String,
        /// System name shown on the sign-on screen.
        #[arg(long, env = "AS4TAINR_SYSTEM_NAME", default_value = "AS4TAINR")]
        system_name: String,
        /// Also serve the browser 5250 terminal on this address (HTTP), for
        /// example 0.0.0.0:8080. Off unless set.
        #[arg(long, env = "AS4TAINR_WEB_LISTEN")]
        web_listen: Option<String>,
    },
    /// CALL: run a program (CL or RPG) as QSECOFR, outside any job.
    Call { program: String, args: Vec<String> },
}

fn split_lib_file(q: &str) -> Result<(String, String)> {
    match q.split_once('/') {
        Some((l, f)) => Ok((l.to_string(), f.to_string())),
        None => bail!("expected a qualified name LIB/FILE, got '{q}'"),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(e) => {
            eprintln!("as4tainr: {e:#}");
            std::process::ExitCode::from(1)
        }
    }
}

/// Runs one command; returns the process exit code.
async fn run(cli: Cli) -> Result<u8> {
    let libl: Vec<String> = cli
        .libl
        .iter()
        .map(|l| catalog::validate_name("library", l))
        .collect::<Result<_>>()?;

    match cli.cmd {
        Cmd::Init => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            println!("as4tainr catalog ready.");
        }
        Cmd::Rstpwd { user } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            let user = user.to_ascii_uppercase();
            if !users::reset_password(&client, &user).await? {
                bail!("CPF2204: User profile {user} not found.");
            }
            println!(
                "Password of {user} reset to {user} (expired: it must be changed at the next sign-on)."
            );
        }
        Cmd::Crtlib { library, text } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::create_library(&client, &library, &text).await?;
            println!("Library {} created.", library.to_ascii_uppercase());
        }
        Cmd::Crtbndcl { program, src, text } => {
            let (lib, name) = split_lib_file(&program)?;
            let source = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let mut client = runtime::connect_db(&cli.database_url).await?;
            let mut libl = vec![lib.to_ascii_uppercase()];
            libl.extend(commands::USER_LIBRARY_LIST.iter().map(|l| l.to_string()));
            let pgm = as4tainr_core::clpgm::compile_in(&client, &libl, &name, &source).await?;
            catalog::create_cl_program(&mut client, &lib, &name, &text, &source, true).await?;
            println!(
                "Program {} created in library {} ({} parameters, {} variables).",
                pgm.name,
                lib.to_ascii_uppercase(),
                pgm.params.len(),
                pgm.decls.len()
            );
        }
        Cmd::Sql { statement } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            if let Some(msg) = as4tainr_core::ddl::try_run(
                &cli.database_url,
                "QSECOFR",
                &libl,
                libl.first().cloned(),
                &mut None,
                &statement,
            )
            .await?
            {
                println!("{msg}");
                return Ok(0);
            }
            match as4tainr_core::sql::run(&client, &cli.database_url, &libl, &statement).await? {
                as4tainr_core::sql::Outcome::Rows {
                    columns,
                    numeric,
                    rows,
                } => {
                    let (head, lines) = as4tainr_core::sql::format_rows(&columns, &numeric, &rows);
                    println!("{head}");
                    for l in lines {
                        println!("{l}");
                    }
                    println!("{} rows selected.", rows.len());
                }
                as4tainr_core::sql::Outcome::Count(kind, n) => {
                    println!("{}", as4tainr_core::sql::count_message(kind, n))
                }
                as4tainr_core::sql::Outcome::Message(m) => println!("{m}"),
            }
        }
        Cmd::Crtlf { file, src } => {
            let (lib, name) = split_lib_file(&file)?;
            let source = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let lf = dds::parse_lf(&name, &source)?;
            let mut client = runtime::connect_db(&cli.database_url).await?;
            catalog::create_logical_file(&mut client, &libl, &lib, &lf).await?;
            let keys: Vec<String> = lf
                .keys
                .iter()
                .map(|k| {
                    if lf.descending.contains(k) {
                        format!("{k} (descending)")
                    } else {
                        k.clone()
                    }
                })
                .collect();
            println!(
                "Logical file {} created in library {} over {} (key: {}{}).",
                lf.name,
                lib.to_ascii_uppercase(),
                lf.based_on,
                if keys.is_empty() {
                    "none".to_string()
                } else {
                    keys.join(", ")
                },
                if lf.select_omit.is_empty() {
                    ""
                } else {
                    "; with select/omit"
                }
            );
        }
        Cmd::Crtpf { file, src } => {
            let (lib, name) = split_lib_file(&file)?;
            let source = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let pf = dds::parse_pf(&name, &source)?;
            let mut client = runtime::connect_db(&cli.database_url).await?;
            catalog::create_physical_file(&mut client, &lib, &pf).await?;
            println!(
                "File {} created in library {} ({} fields, key: {}).",
                pf.name,
                lib.to_ascii_uppercase(),
                pf.fields.len(),
                if pf.keys.is_empty() {
                    "none".to_string()
                } else {
                    pf.keys.join(", ")
                }
            );
        }
        Cmd::Crtdspf { file, src } => {
            let (lib, name) = split_lib_file(&file)?;
            let source = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let mut client = runtime::connect_db(&cli.database_url).await?;
            let f = catalog::create_display_file(&mut client, &lib, &name, &source).await?;
            println!(
                "Display file {} created in library {} (record formats: {}).",
                f.name,
                lib.to_ascii_uppercase(),
                f.records
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Cmd::Load { file, csv } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            let f = catalog::resolve_file(&client, &libl, &file).await?;
            let text = std::fs::read_to_string(&csv)
                .with_context(|| format!("reading {}", csv.display()))?;
            let (lib, name) = (f.library.clone(), f.file.name.clone());
            let n = as4tainr_core::import::load_csv(&client, f, &text).await?;
            println!("{n} records written to {lib}/{name}.");
        }
        Cmd::Put { from, to, ccsid } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            let mut n = 0;
            let mut todo = vec![(from.clone(), as4tainr_core::ifs::normalize(&to)?)];
            while let Some((local, path)) = todo.pop() {
                if local.is_dir() {
                    as4tainr_core::ifs::mkdir_all(&client, &path, "QSECOFR").await?;
                    for e in std::fs::read_dir(&local)? {
                        let e = e?;
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.starts_with('.') {
                            continue;
                        }
                        todo.push((e.path(), format!("{}/{name}", path.trim_end_matches('/'))));
                    }
                } else {
                    let data = std::fs::read(&local)?;
                    let parent = path.rsplit_once('/').map(|x| x.0).unwrap_or("/");
                    as4tainr_core::ifs::mkdir_all(
                        &client,
                        if parent.is_empty() { "/" } else { parent },
                        "QSECOFR",
                    )
                    .await?;
                    as4tainr_core::ifs::write_file(&client, &path, &data, ccsid, 0o644, "QSECOFR")
                        .await?;
                    n += 1;
                }
            }
            println!("{n} stream files copied to {to}.");
        }
        Cmd::Get { from, to } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            let (data, _) = as4tainr_core::ifs::read_file(&client, &from).await?;
            std::fs::write(&to, &data)?;
            println!(
                "{} bytes copied from {from} to {}.",
                data.len(),
                to.display()
            );
        }
        Cmd::Import { dir, lib } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            let env = commands::Environment {
                database_url: cli.database_url.clone(),
            };
            let mut job = commands::Job {
                client: &client,
                env: &env,
                user: "QSECOFR".into(),
                curlib: None,
                libl: libl.clone(),
                display: None,
                id: None,
                calls: Default::default(),
            };
            let report = as4tainr_core::import::import(&mut job, &dir, &lib).await?;
            println!(
                "Import of {} into {}:",
                dir.display(),
                lib.to_ascii_uppercase()
            );
            for l in &report.lines {
                println!("{l}");
            }
            if report.failed > 0 {
                println!("{} objects could not be created.", report.failed);
                return Ok(1);
            }
            println!("All objects created.");
        }
        Cmd::System { args } => {
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            let env = commands::Environment {
                database_url: cli.database_url.clone(),
            };
            let mut job = commands::Job {
                client: &client,
                env: &env,
                user: "QSECOFR".into(),
                curlib: None,
                libl: libl.clone(),
                display: None,
                id: None,
                calls: Default::default(),
            };
            let out = as4tainr_core::pase_system::run(&mut job, &args).await;
            for l in &out.stdout {
                println!("{l}");
            }
            for l in &out.stderr {
                eprintln!("{l}");
            }
            return Ok(out.status);
        }
        Cmd::Tn5250 {
            listen,
            system_name,
            web_listen,
        } => {
            // Make sure the catalog and QSECOFR exist before anyone signs on.
            let client = runtime::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            // Jobs left active by a server that stopped are not running.
            let stale = as4tainr_core::jobs::recover(&client).await?;
            if stale > 0 {
                println!("as4tainr: {stale} jobs from before the restart marked as ended");
            }
            drop(client);
            let listener = tokio::net::TcpListener::bind(&listen)
                .await
                .with_context(|| format!("listening on {listen}"))?;
            println!(
                "as4tainr 5250 server listening on {} (system {system_name})",
                listener.local_addr()?
            );
            // The browser terminal connects to this listener, in this process.
            let web = match web_listen.filter(|w| !w.is_empty()) {
                Some(w) => {
                    let web = tokio::net::TcpListener::bind(&w)
                        .await
                        .with_context(|| format!("listening on {w}"))?;
                    println!(
                        "as4tainr web terminal listening on http://{}",
                        web.local_addr()?
                    );
                    let backend = as4tainr_core::web::loopback_for(listener.local_addr()?);
                    Some(tokio::spawn(as4tainr_core::web::serve(web, backend)))
                }
                None => None,
            };
            let web = async move {
                match web {
                    Some(t) => t.await?,
                    None => std::future::pending().await,
                }
            };
            let cfg = tn5250::ServerConfig {
                env: commands::Environment {
                    database_url: cli.database_url,
                },
                system_name: system_name.to_ascii_uppercase(),
                libl: libl.clone(),
                max_signon_attempts: 3,
            };
            // The QBATCH subsystem runs submitted jobs in this process.
            let maxact = as4tainr_core::jobs::batch_max_active();
            println!("as4tainr: subsystem QBATCH started (MAXACT {maxact})");
            let qbatch = tokio::spawn(as4tainr_core::jobs::run_subsystem(cfg.env.clone(), maxact));
            tokio::select! {
                r = tn5250::serve(listener, cfg) => r?,
                r = qbatch => r??,
                r = web => r?,
            }
        }
        Cmd::Call { program, args } => {
            // A CL program runs here as a job would run it; an RPG program
            // on the RPG engine. Printed output goes to standard output.
            let client = runtime::connect_db(&cli.database_url).await?;
            let env = commands::Environment {
                database_url: cli.database_url.clone(),
            };
            // An RPG program's DSPLY output is printed as it wrote it; a CL
            // program's messages as its job log would show them.
            let is_cl = {
                let name = program.to_ascii_uppercase();
                let (libs, n) = match name.split_once('/') {
                    Some((l, n)) => (vec![l.to_string()], n.to_string()),
                    None => (libl.clone(), name.clone()),
                };
                catalog::find_cl_program(&client, &libs, &n)
                    .await?
                    .is_some()
            };
            let show = |l: &str| -> String {
                match l.strip_prefix("DSPLY  ") {
                    Some(t) if !is_cl => t.to_string(),
                    _ => l.to_string(),
                }
            };
            let mut job = commands::Job {
                client: &client,
                env: &env,
                user: "QSECOFR".into(),
                curlib: None,
                libl: libl.clone(),
                display: None,
                id: None,
                calls: Default::default(),
            };
            match job.call_program(&program, args).await {
                Ok(commands::Outcome::Messages(m)) => {
                    for l in m {
                        println!("{}", show(&l));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    let text = format!("{e:#}");
                    let mut lines: Vec<&str> = text.lines().collect();
                    // DSPLY lines before the error are output too.
                    while lines.first().is_some_and(|l| l.starts_with("DSPLY  ")) {
                        println!("{}", show(lines.remove(0)));
                    }
                    for l in lines {
                        eprintln!("as4tainr: {l}");
                    }
                    return Ok(1);
                }
            }
        }
    }
    Ok(0)
}
