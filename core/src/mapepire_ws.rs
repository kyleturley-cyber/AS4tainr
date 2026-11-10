//! Mapepire's server mode: the Mapepire protocol (mapepire.rs) over
//! secure WebSockets, as IBM's Mapepire server daemon serves it on port
//! 8076 (`wss://host:8076/db/`, a user profile and its password in HTTP
//! Basic authentication; each request and reply a text message). Clients
//! such as mapepire-js, mapepire-python and Code for IBM i ("Mapepire: use
//! server") connect to it.
//!
//! TLS uses the certificate and key in AS4TAINR_MAPEPIRE_CERT and
//! AS4TAINR_MAPEPIRE_KEY (PEM files), else a self-signed one made the first
//! time and kept in the database (`as4tainr.tls_certificates`), as the
//! daemon makes its own when it has none.

use anyhow::{anyhow, bail, Result};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.tls_certificates (
    name text PRIMARY KEY,
    cert text NOT NULL,
    key  text NOT NULL
);
"#;

/// Serve Mapepire on `listen` until the process ends.
pub async fn run(database_url: String, listen: String) -> Result<()> {
    let client = crate::runtime::connect_db(&database_url).await?;
    crate::catalog::init(&client).await?;
    client.batch_execute(DDL).await?;
    let (cert, key) = certificate(&client).await?;
    let config = tls_config(&cert, &key)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("as4tainr: Mapepire server listening on {listen}");
    let url = Arc::new(database_url);
    loop {
        let (sock, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let url = url.clone();
        tokio::spawn(async move {
            match acceptor.accept(sock).await {
                Ok(tls) => {
                    if let Err(e) = connection(&url, tls).await {
                        eprintln!("as4tainr: Mapepire connection ended: {e:#}");
                    }
                }
                Err(e) => eprintln!("as4tainr: Mapepire TLS handshake failed: {e}"),
            }
        });
    }
}

/// The certificate and key (PEM): from the files named, else the kept
/// self-signed one, made the first time.
async fn certificate(client: &Client) -> Result<(String, String)> {
    if let (Ok(c), Ok(k)) = (
        std::env::var("AS4TAINR_MAPEPIRE_CERT"),
        std::env::var("AS4TAINR_MAPEPIRE_KEY"),
    ) {
        return Ok((std::fs::read_to_string(c)?, std::fs::read_to_string(k)?));
    }
    let q = "SELECT cert, key FROM as4tainr.tls_certificates WHERE name = 'mapepire'";
    if let Some(r) = client.query_opt(q, &[]).await? {
        return Ok((r.get(0), r.get(1)));
    }
    let host = std::env::var("AS4TAINR_SYSTEM_NAME").unwrap_or_else(|_| "as4tainr".into());
    let names = vec![host, "localhost".to_string()];
    let made = rcgen::generate_simple_self_signed(names)?;
    let cert = made.cert.pem();
    let key = made.key_pair.serialize_pem();
    client
        .execute(
            "INSERT INTO as4tainr.tls_certificates (name, cert, key) VALUES ('mapepire', $1, $2)
             ON CONFLICT DO NOTHING",
            &[&cert, &key],
        )
        .await?;
    let r = client.query_one(q, &[]).await?;
    Ok((r.get(0), r.get(1)))
}

fn tls_config(cert: &str, key: &str) -> Result<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert.as_bytes())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow!("the Mapepire certificate is not valid: {e}"))?;
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes())
        .map_err(|e| anyhow!("the Mapepire key is not valid: {e}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(config)
}

/// The user and password of an `Authorization: Basic` header.
pub fn basic_credentials(header: &str) -> Option<(String, String)> {
    let b64 = header
        .trim()
        .strip_prefix("Basic ")
        .or_else(|| header.trim().strip_prefix("basic "))?;
    let raw = base64_decode(b64.trim())?;
    let text = String::from_utf8(raw).ok()?;
    let (u, p) = text.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// One client: the upgrade to a WebSocket (signed on with the request's
/// credentials), then Mapepire requests until it closes.
pub async fn connection<S>(url: &str, stream: S) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let mut rd = tokio::io::BufReader::new(rd);
    // The request line and headers.
    let mut request = String::new();
    rd.read_line(&mut request).await?;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut l = String::new();
        if rd.read_line(&mut l).await? == 0 {
            bail!("the connection closed during the request");
        }
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            bail!("too many headers");
        }
    }
    let header = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let (Some(key), true) = (
        header("sec-websocket-key"),
        path.trim_end_matches('/') == "/db",
    ) else {
        wr.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    };
    let client = crate::runtime::connect_db(url).await?;
    let user = match header("authorization").and_then(|h| basic_credentials(&h)) {
        Some((u, p)) => match crate::users::authenticate(&client, &u, &p).await {
            Ok(prof) if !prof.pwd_expired => Some(prof.name),
            _ => None,
        },
        None => None,
    };
    let Some(user) = user else {
        wr.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"mapepire\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
        return Ok(());
    };
    wr.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            crate::web::ws::accept_key(&key)
        )
        .as_bytes(),
    )
    .await?;
    wr.flush().await?;
    // The protocol engine reads lines and writes lines: bridged to the
    // WebSocket's messages.
    let (to_engine, engine_in) = tokio::io::duplex(1 << 20);
    let (engine_out, from_engine) = tokio::io::duplex(1 << 20);
    let env = crate::commands::Environment {
        database_url: url.to_string(),
    };
    let libl: Vec<String> = crate::commands::USER_LIBRARY_LIST
        .iter()
        .map(|l| l.to_string())
        .collect();
    // The daemon answers as the 2.3 servers do (verify64-71 on PUB400).
    let legacy = std::env::var("AS4TAINR_MAPEPIRE_VERSION")
        .map(|v| crate::mapepire::legacy_cl_for(&[format!("mapepire-server-{v}.jar")]))
        .unwrap_or(true);
    let engine = async move {
        crate::mapepire::serve(&client, &env, &user, libl, legacy, engine_in, engine_out).await
    };
    let reader = async move {
        let mut to_engine = to_engine;
        loop {
            match crate::web::ws::read_message(&mut rd).await {
                Ok(crate::web::ws::Message::Text(t)) => {
                    // One request a line.
                    let t = t.replace('\n', " ");
                    to_engine.write_all(t.as_bytes()).await?;
                    to_engine.write_all(b"\n").await?;
                    to_engine.flush().await?;
                }
                Ok(crate::web::ws::Message::Close) | Err(_) => break,
                Ok(_) => {}
            }
        }
        drop(to_engine);
        Ok::<_, anyhow::Error>(())
    };
    let writer = async move {
        let mut lines = tokio::io::BufReader::new(from_engine).lines();
        while let Some(l) = lines.next_line().await? {
            crate::web::ws::send_text(&mut wr, &l).await?;
            wr.flush().await?;
        }
        let _ = crate::web::ws::send_close(&mut wr).await;
        let _ = wr.flush().await;
        Ok::<_, anyhow::Error>(())
    };
    let (e, r, w) = tokio::join!(engine, reader, writer);
    e.and(r).and(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_from_basic_authorization() {
        // RFC 7617: "Basic " and base64 of user:password.
        assert_eq!(
            basic_credentials("Basic TUU6c2VjcmV0"),
            Some(("ME".to_string(), "secret".to_string()))
        );
        assert_eq!(basic_credentials("Bearer x"), None);
    }
}
