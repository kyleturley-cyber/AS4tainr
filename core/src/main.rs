use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ibmish_core::{bridge, catalog, commands, dds, rla, tn5250};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ibmish",
    version,
    about = "ibmish: an IBM i-compatible runtime"
)]
struct Cli {
    /// PostgreSQL connection URL.
    #[arg(
        long,
        global = true,
        env = "IBMISH_DATABASE_URL",
        default_value = "postgres://ibmish:ibmish@127.0.0.1/ibmish"
    )]
    database_url: String,

    /// Library list, comma-separated, searched in order.
    #[arg(
        long,
        global = true,
        env = "IBMISH_LIBL",
        default_value = "QGPL",
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
    /// Load records from a CSV file (header row = field names) with WRITE.
    Load {
        /// File name, optionally qualified LIB/FILE.
        file: String,
        #[arg(long)]
        csv: PathBuf,
    },
    /// Run the JVM bridge server.
    Serve {
        #[arg(long, default_value_t = 7250)]
        port: u16,
    },
    /// Run the TN5250 display server (5250 green-screen sessions).
    Tn5250 {
        /// Address to listen on.
        #[arg(long, env = "IBMISH_TN5250_LISTEN", default_value = "0.0.0.0:23")]
        listen: String,
        /// System name shown on the sign-on screen.
        #[arg(long, env = "IBMISH_SYSTEM_NAME", default_value = "IBMISH")]
        system_name: String,
        /// Command that runs RPG programs for CALL.
        #[arg(
            long,
            env = "IBMISH_RUNNER",
            default_value = "java -jar /opt/ibmish/ibmish-jvm.jar"
        )]
        runner: String,
    },
    /// CALL: run a program. Starts a bridge on a free port and launches the
    /// runner (the JVM with Jariko), passing `--bridge HOST:PORT` first.
    Call {
        program: String,
        args: Vec<String>,
        /// Command that runs RPG programs.
        #[arg(
            long,
            env = "IBMISH_RUNNER",
            default_value = "java -jar /opt/ibmish/ibmish-jvm.jar"
        )]
        runner: String,
    },
}

fn split_lib_file(q: &str) -> Result<(String, String)> {
    match q.split_once('/') {
        Some((l, f)) => Ok((l.to_string(), f.to_string())),
        None => bail!("expected a qualified name LIB/FILE, got '{q}'"),
    }
}

/// Minimal CSV: comma-separated, double quotes for fields containing
/// commas or quotes ("" escapes a quote).
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => std::process::ExitCode::from(code),
        Err(e) => {
            eprintln!("ibmish: {e:#}");
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
            let client = bridge::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            println!("ibmish catalog ready.");
        }
        Cmd::Crtlib { library, text } => {
            let client = bridge::connect_db(&cli.database_url).await?;
            catalog::create_library(&client, &library, &text).await?;
            println!("Library {} created.", library.to_ascii_uppercase());
        }
        Cmd::Crtpf { file, src } => {
            let (lib, name) = split_lib_file(&file)?;
            let source = std::fs::read_to_string(&src)
                .with_context(|| format!("reading {}", src.display()))?;
            let pf = dds::parse_pf(&name, &source)?;
            let mut client = bridge::connect_db(&cli.database_url).await?;
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
        Cmd::Load { file, csv } => {
            let client = bridge::connect_db(&cli.database_url).await?;
            let f = catalog::resolve_file(&client, &libl, &file).await?;
            let text = std::fs::read_to_string(&csv)
                .with_context(|| format!("reading {}", csv.display()))?;
            let mut lines = text.lines().filter(|l| !l.trim().is_empty());
            let header: Vec<String> = parse_csv_line(lines.next().context("empty CSV")?)
                .into_iter()
                .map(|h| h.trim().to_ascii_uppercase())
                .collect();
            let mut of = rla::OpenFile::new(f);
            let mut n = 0;
            for line in lines {
                let vals = parse_csv_line(line);
                let rec: rla::Record = header.iter().cloned().zip(vals).collect();
                of.write(&client, &rec).await?;
                n += 1;
            }
            println!(
                "{n} records written to {}/{}.",
                of.file.library, of.file.file.name
            );
        }
        Cmd::Serve { port } => {
            let listener = bridge::bind(port).await?;
            println!("ibmish bridge listening on {}", listener.local_addr()?);
            bridge::serve(
                listener,
                bridge::BridgeConfig {
                    database_url: cli.database_url,
                    library_list: libl,
                },
            )
            .await?;
        }
        Cmd::Tn5250 {
            listen,
            system_name,
            runner,
        } => {
            // Make sure the catalog and QSECOFR exist before anyone signs on.
            let client = bridge::connect_db(&cli.database_url).await?;
            catalog::init(&client).await?;
            drop(client);
            let listener = tokio::net::TcpListener::bind(&listen)
                .await
                .with_context(|| format!("listening on {listen}"))?;
            println!(
                "ibmish 5250 server listening on {} (system {system_name})",
                listener.local_addr()?
            );
            let cfg = tn5250::ServerConfig {
                env: commands::Environment {
                    database_url: cli.database_url,
                    runner,
                    call_timeout: std::time::Duration::from_secs(120),
                },
                system_name: system_name.to_ascii_uppercase(),
                libl: libl.clone(),
                max_signon_attempts: 3,
            };
            tn5250::serve(listener, cfg).await?;
        }
        Cmd::Call {
            program,
            args,
            runner,
        } => {
            let listener = bridge::bind(0).await?;
            let addr = listener.local_addr()?;
            let cfg = bridge::BridgeConfig {
                database_url: cli.database_url,
                library_list: libl,
            };
            let server = tokio::spawn(bridge::serve(listener, cfg));

            let mut parts = runner.split_whitespace();
            let exe = parts.next().context("empty --runner")?;
            let status = tokio::process::Command::new(exe)
                .args(parts)
                .arg("--bridge")
                .arg(addr.to_string())
                .arg(&program)
                .args(&args)
                .status()
                .await
                .with_context(|| format!("starting runner '{runner}'"))?;
            server.abort();
            if !status.success() {
                eprintln!(
                    "ibmish: program {} ended abnormally ({status})",
                    program.to_ascii_uppercase()
                );
                return Ok(status.code().map(|c| c.clamp(1, 255) as u8).unwrap_or(1));
            }
        }
    }
    Ok(0)
}
