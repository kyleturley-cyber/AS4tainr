//! Trigger programs on physical files (ADDPFTRG, RMVPFTRG).
//!
//! Written from IBM's CL command reference (ADDPFTRG, RMVPFTRG) and the
//! Db2 for i database programming topics on trigger programs ("Trigger
//! buffer sections"). A trigger program is called with two parameters:
//! the trigger buffer and its length. The buffer starts with a fixed part
//! (file, library and member names, the event — '1' insert, '2' delete,
//! '3' update — the time — '1' after, '2' before — the commit lock level
//! — '0' *NONE, '1' *CHG, '2' *CS, '3' *ALL —, the CCSID, the relative
//! record number, and the offsets and lengths of the record images and
//! null byte maps), then the original record, its null byte map, the new
//! record and its null byte map, each in the file's record format.
//!
//! PostgreSQL row triggers on the file note each change in a list kept by
//! the database session; as4tainr then calls the trigger programs for
//! them, in order, before the change is committed. A trigger program that
//! ends in error (an escape message) undoes the change: the program that
//! made it gets CPF502B (RPG file status 1023 or 1024), an SQL statement
//! SQL0443.

use crate::catalog::{self, ident};
use crate::cl::Command;
use crate::commands::{Job, Outcome};
use anyhow::{anyhow, bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.file_triggers (
    library   text NOT NULL,
    file      text NOT NULL,
    trg_lib   text NOT NULL,
    trg       text NOT NULL,
    time      text NOT NULL,
    event     text NOT NULL,
    pgm_lib   text NOT NULL,
    pgm       text NOT NULL,
    alwrepchg boolean NOT NULL DEFAULT false,
    updcnd    text NOT NULL DEFAULT '*ALWAYS',
    added     timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (library, file, trg)
);
"#;

/// PostgreSQL functions (made with the IBM i services): the row trigger
/// that notes changes for trigger programs, and taking the noted changes.
pub fn sql_functions() -> String {
    format!(
        r#"
-- TG_ARGV[0] = 'B' (before) or 'A' (after); [1], [2] the file.
CREATE OR REPLACE FUNCTION as4tainr_svc.pftrg_capture() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  t record;
  ev text := CASE TG_OP WHEN 'INSERT' THEN '*INSERT' WHEN 'DELETE' THEN '*DELETE' ELSE '*UPDATE' END;
  tm text := CASE TG_ARGV[0] WHEN 'B' THEN '*BEFORE' ELSE '*AFTER' END;
  o record;
  n record;
  q record;
  oj jsonb := CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END;
  nj jsonb := CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END;
BEGIN
  -- APYJRNCHG and RMVJRNCHG call no trigger programs.
  IF current_setting('as4tainr.apply', true) = '1' THEN
    IF TG_ARGV[0] = 'A' THEN RETURN NULL; END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
  END IF;
  -- The change made again after its *BEFORE trigger programs ran: only
  -- the *AFTER ones are noted.
  IF TG_ARGV[0] = 'B' AND current_setting('as4tainr.pftrg_skip_before', true) = '1' THEN
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    -- An inserted record keeps the relative record number it had when
    -- the programs were called; a record they changed is written so.
    IF to_regclass('pg_temp.as4tainr_trg_redo') IS NOT NULL THEN
      DELETE FROM pg_temp.as4tainr_trg_redo r WHERE r.seq = (SELECT min(x.seq) FROM pg_temp.as4tainr_trg_redo x
          WHERE x.lib = TG_ARGV[1] AND x.file = TG_ARGV[2] AND x.op = TG_OP
            AND (TG_OP = 'INSERT' OR x.rrn = (oj ->> 'rrn')::bigint))
        RETURNING r.rrn, r.rowj INTO q;
      IF FOUND THEN
        NEW := jsonb_populate_record(NEW, coalesce(q.rowj, '{{}}'::jsonb)
            || CASE WHEN TG_OP = 'INSERT' AND q.rrn IS NOT NULL THEN jsonb_build_object('rrn', q.rrn)
                    ELSE '{{}}'::jsonb END);
      END IF;
    END IF;
    RETURN NEW;
  END IF;
  -- A change made with trigger programs: one that has *BEFORE programs
  -- is made on trial first (as4tainr.pftrg_trial).
  IF TG_ARGV[0] = 'B' AND current_setting('as4tainr.pftrg_probe', true) = '1'
     AND coalesce(current_setting('as4tainr.pftrg_trial', true), '') <> '1'
     AND EXISTS (SELECT 1 FROM as4tainr.file_triggers
                  WHERE library = TG_ARGV[1] AND file = TG_ARGV[2] AND time = '*BEFORE' AND event = ev) THEN
    RAISE EXCEPTION '{PROBE}' USING ERRCODE = 'ZT001';
  END IF;
  -- The change made on trial (to call the *BEFORE programs): only they
  -- are noted.
  IF TG_ARGV[0] = 'A' AND current_setting('as4tainr.pftrg_trial', true) = '1' THEN
    RETURN NULL;
  END IF;
  FOR t IN SELECT * FROM as4tainr.file_triggers
            WHERE library = TG_ARGV[1] AND file = TG_ARGV[2] AND time = tm AND event = ev
            ORDER BY added LOOP
    IF ev = '*UPDATE' AND t.updcnd = '*CHANGE' AND oj = nj THEN CONTINUE; END IF;
    CREATE TEMP TABLE IF NOT EXISTS as4tainr_trg_events (
      seq bigserial, pgm_lib text, pgm text, trg_time char(1), trg_event char(1), lib text, file text,
      rrn bigint, cmt char(1), old_image bytea, old_nulls text, new_image bytea, new_nulls text,
      alwrepchg boolean);
    SELECT * INTO o FROM as4tainr_svc.record_image(TG_ARGV[1], TG_ARGV[2], oj);
    SELECT * INTO n FROM as4tainr_svc.record_image(TG_ARGV[1], TG_ARGV[2], nj);
    INSERT INTO pg_temp.as4tainr_trg_events (pgm_lib, pgm, trg_time, trg_event, lib, file, rrn, cmt,
        old_image, old_nulls, new_image, new_nulls, alwrepchg)
    VALUES (t.pgm_lib, t.pgm, CASE tm WHEN '*BEFORE' THEN '2' ELSE '1' END,
        CASE ev WHEN '*INSERT' THEN '1' WHEN '*DELETE' THEN '2' ELSE '3' END,
        TG_ARGV[1], TG_ARGV[2], coalesce(nj ->> 'rrn', oj ->> 'rrn')::bigint,
        CASE WHEN current_setting('as4tainr.cmt', true) = '1' THEN '1' ELSE '0' END,
        o.image, o.nulls, n.image, n.nulls, t.alwrepchg);
  END LOOP;
  IF TG_ARGV[0] = 'B' THEN
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
  END IF;
  RETURN NULL;
END $as4tainr$;

-- The changes noted since the last call, oldest first; the list is
-- emptied.
DROP FUNCTION IF EXISTS as4tainr_svc.pftrg_take();
CREATE OR REPLACE FUNCTION as4tainr_svc.pftrg_take(OUT pgm_lib text, OUT pgm text, OUT trg_time text,
    OUT trg_event text, OUT lib text, OUT file text, OUT rrn bigint, OUT cmt text, OUT old_image bytea,
    OUT old_nulls text, OUT new_image bytea, OUT new_nulls text, OUT alwrepchg boolean) RETURNS SETOF record
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  IF to_regclass('pg_temp.as4tainr_trg_events') IS NULL THEN RETURN; END IF;
  RETURN QUERY
    WITH d AS (DELETE FROM pg_temp.as4tainr_trg_events RETURNING *)
    SELECT d.pgm_lib, d.pgm, d.trg_time::text, d.trg_event::text, d.lib, d.file, d.rrn, d.cmt::text,
           d.old_image, d.old_nulls, d.new_image, d.new_nulls, d.alwrepchg FROM d ORDER BY d.seq;
END $as4tainr$;

-- Has the file (or the physical file a logical file is over) trigger
-- programs?
CREATE OR REPLACE FUNCTION as4tainr_svc.has_pftrg(lib text, f text) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT EXISTS (SELECT 1 FROM as4tainr.file_triggers t
                  WHERE (t.library = lib AND t.file = f)
                     OR (t.library, t.file) IN (SELECT pf_library, pf_name FROM as4tainr.logical_files
                                                 WHERE library = lib AND name = f))
$as4tainr$;

-- Notes for making a change again after its *BEFORE programs ran (l
-- NULL: forget them).
CREATE OR REPLACE FUNCTION as4tainr_svc.pftrg_redo(l text, f text, o text, r bigint, j jsonb) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  CREATE TEMP TABLE IF NOT EXISTS as4tainr_trg_redo (seq bigserial, lib text, file text, op text, rrn bigint, rowj jsonb);
  IF l IS NULL THEN DELETE FROM pg_temp.as4tainr_trg_redo; RETURN; END IF;
  INSERT INTO pg_temp.as4tainr_trg_redo (lib, file, op, rrn, rowj) VALUES (l, f, o, r, j);
END $as4tainr$;

-- Has any file *BEFORE trigger programs?
CREATE OR REPLACE FUNCTION as4tainr_svc.any_pftrg_before() RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT EXISTS (SELECT 1 FROM as4tainr.file_triggers WHERE time = '*BEFORE')
$as4tainr$;

-- Has any file trigger programs?
CREATE OR REPLACE FUNCTION as4tainr_svc.any_pftrg() RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT EXISTS (SELECT 1 FROM as4tainr.file_triggers)
$as4tainr$;

GRANT EXECUTE ON FUNCTION as4tainr_svc.pftrg_take(), as4tainr_svc.has_pftrg(text, text),
    as4tainr_svc.any_pftrg(), as4tainr_svc.any_pftrg_before(),
    as4tainr_svc.pftrg_redo(text, text, text, bigint, jsonb) TO {role};
"#,
        role = crate::sql::SQL_ROLE,
        PROBE = PROBE,
    )
}

/// The error a change gets when it has *BEFORE trigger programs and is
/// not made on trial.
const PROBE: &str = "as4tainr-pftrg-before";

/// A change noted for a trigger program.
#[derive(Debug, Clone)]
pub struct Event {
    pub pgm_lib: String,
    pub pgm: String,
    pub time: String,
    pub event: String,
    pub lib: String,
    pub file: String,
    pub rrn: i64,
    pub cmt: String,
    pub old: Option<(Vec<u8>, String)>,
    pub new: Option<(Vec<u8>, String)>,
    /// ALWREPCHG(*YES): a *BEFORE program may change the new record.
    pub alwrepchg: bool,
}

/// The changes noted on this database session since the last call.
pub async fn take(client: &Client) -> Result<Vec<Event>> {
    let rows = client
        .query("SELECT * FROM as4tainr_svc.pftrg_take()", &[])
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let img = |i: usize, n: usize| -> Option<(Vec<u8>, String)> {
                r.get::<_, Option<Vec<u8>>>(i)
                    .map(|b| (b, r.get::<_, Option<String>>(n).unwrap_or_default()))
            };
            Event {
                pgm_lib: r.get(0),
                pgm: r.get(1),
                time: r.get(2),
                event: r.get(3),
                lib: r.get(4),
                file: r.get(5),
                rrn: r.get::<_, Option<i64>>(6).unwrap_or(0),
                cmt: r.get(7),
                old: img(8, 9),
                new: img(10, 11),
                alwrepchg: r.get::<_, Option<bool>>(12).unwrap_or(false),
            }
        })
        .collect())
}

/// The trigger buffer for a change (Db2 for i, "Trigger buffer
/// sections"): the 96-byte static section, then the original record and
/// its null byte map, the new record and its null byte map.
pub fn buffer(e: &Event) -> Vec<u8> {
    let enc = |s: &str, n: usize| -> Vec<u8> {
        let mut b = crate::tn5250::ebcdic::encode(&format!("{s:<n$}"));
        b.truncate(n);
        b
    };
    let mut b = Vec::with_capacity(512);
    b.extend(enc(&e.file, 10));
    b.extend(enc(&e.lib, 10));
    b.extend(enc(&e.file, 10));
    b.extend(enc(&e.event, 1));
    b.extend(enc(&e.time, 1));
    b.extend(enc(&e.cmt, 1));
    b.extend([0x40u8; 3]);
    b.extend(37i32.to_be_bytes());
    b.extend((e.rrn as i32).to_be_bytes());
    b.extend([0u8; 4]);
    b.resize(96, 0);
    let put = |b: &mut Vec<u8>, img: &Option<(Vec<u8>, String)>, at: usize| {
        let (rec, nulls) = match img {
            Some((r, n)) => (r.clone(), crate::tn5250::ebcdic::encode(n)),
            None => (Vec::new(), Vec::new()),
        };
        let off = b.len() as i32;
        b[at..at + 4].copy_from_slice(&(if rec.is_empty() { 0 } else { off }).to_be_bytes());
        b[at + 4..at + 8].copy_from_slice(&(rec.len() as i32).to_be_bytes());
        b.extend(&rec);
        let noff = b.len() as i32;
        b[at + 8..at + 12]
            .copy_from_slice(&(if nulls.is_empty() { 0 } else { noff }).to_be_bytes());
        b[at + 12..at + 16].copy_from_slice(&(nulls.len() as i32).to_be_bytes());
        b.extend(&nulls);
    };
    put(&mut b, &e.old, 48);
    put(&mut b, &e.new, 64);
    b
}

/// The job a trigger program runs for.
pub struct Caller<'a> {
    pub database_url: &'a str,
    pub libl: Vec<String>,
    pub user: String,
    pub job: Option<String>,
    pub curlib: Option<String>,
}

/// Has any file trigger programs? (Statements skip the work otherwise.)
pub async fn any(client: &Client) -> bool {
    client
        .query_one("SELECT as4tainr_svc.any_pftrg()", &[])
        .await
        .map(|r| r.get(0))
        .unwrap_or(false)
}

/// The new record image a *BEFORE trigger program left in the buffer, if
/// it may change it (ALWREPCHG) and did.
pub fn changed_new_image(e: &Event, buf: &[u8]) -> Option<Vec<u8>> {
    if !e.alwrepchg || e.time != "2" {
        return None;
    }
    let (img, _) = e.new.as_ref()?;
    let off = i32::from_be_bytes(buf.get(64..68)?.try_into().ok()?) as usize;
    let got = buf.get(off..off + img.len())?;
    (got != img.as_slice()).then(|| got.to_vec())
}

/// Call one trigger program for a change, in session `data` (None: a
/// session of its own); the buffer as the program left it. An error is
/// CPF502B.
pub async fn call(
    e: &Event,
    catalog: &Client,
    who: &Caller<'_>,
    data: Option<crate::runtime::DataSession>,
) -> Result<Vec<u8>> {
    let p = catalog::find_program(catalog, std::slice::from_ref(&e.pgm_lib), &e.pgm)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "CPF502B: Trigger program {}/{} for file {}/{} not found ({} the change).",
                e.pgm_lib,
                e.pgm,
                e.lib,
                e.file,
                when(e)
            )
        })?;
    if !matches!(p.attribute.as_str(), "RPGLE" | "SQLRPGLE" | "RPG") {
        bail!(
            "CPF502B: Trigger program {}/{} is a {} program; as4tainr calls RPG trigger programs ({} the change).",
            p.library,
            p.name,
            p.attribute,
            when(e)
        );
    }
    let buf = buffer(e);
    let len = (buf.len() as i32).to_be_bytes().to_vec();
    let cl = crate::rpg::ClContext {
        env: crate::commands::Environment {
            database_url: who.database_url.to_string(),
        },
        curlib: who.curlib.clone(),
        user_libl: who.libl.clone(),
        overrides: Vec::new(),
    };
    let back = match &data {
        Some(d) => Some(set_program(d.client(), &p.name, &p.library).await),
        None => None,
    };
    let called = crate::rpg::call_bytes(
        who.database_url,
        who.libl.clone(),
        who.user.clone(),
        who.job.clone(),
        Some(cl),
        p.name.clone(),
        crate::rpg::ProgramCode {
            library: Some(p.library.clone()),
            source: p.source,
            options: p.options,
            binding: p.binding,
        },
        vec![buf, len],
        data.clone(),
    )
    .await;
    if let (Some(d), Some((n, l))) = (&data, back) {
        set_program(d.client(), &n, &l).await;
    }
    let (r, parms) = called?;
    if let Some(err) = r.error {
        bail!(
            "CPF502B: Error in trigger program {}/{} for file {}/{} ({} the change): {err:#}",
            p.library,
            p.name,
            e.lib,
            e.file,
            when(e)
        );
    }
    Ok(parms.into_iter().next().unwrap_or_default())
}

/// The program the journal entries of a session's changes name: set
/// for a trigger program's changes; the one before is returned.
pub async fn set_program(client: &Client, name: &str, lib: &str) -> (String, String) {
    client
        .query_one(
            "SELECT coalesce(current_setting('as4tainr.program', true), ''),
                    coalesce(current_setting('as4tainr.program_lib', true), ''),
                    set_config('as4tainr.program', $1, true), set_config('as4tainr.program_lib', $2, true)",
            &[&name, &lib],
        )
        .await
        .map(|r| (r.get(0), r.get(1)))
        .unwrap_or_default()
}

pub fn when(e: &Event) -> &'static str {
    if e.time == "2" {
        "before"
    } else {
        "after"
    }
}

static SAVEPOINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A savepoint name of its own.
pub fn savepoint() -> String {
    format!(
        "as4tainr_trg{}",
        SAVEPOINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// A change with its trigger programs (Db2 for i, "Trigger programs" and
/// their commitment control rules), made in the open transaction of a
/// session. When any file has *BEFORE trigger programs the change is
/// first made on trial (not journaled, nothing kept), its *BEFORE
/// programs are called with the records it would write, and it is made
/// again with the new records they changed (ALWREPCHG(*YES)); its *AFTER
/// programs are called after it is made. The programs run in the same
/// session. When one ends in error: under commitment control (`cmt`) the
/// change and the programs' changes are taken back (the rolled-back
/// records are journaled); otherwise the files stay as they are when the
/// error happens (a *BEFORE program's error: the change is not made; an
/// *AFTER program's: it stays).
///
/// [`run`] drives it for SQL statements; the RPG engine drives it
/// itself, calling the programs in its own thread.
pub struct Change<'a> {
    client: &'a Client,
    catalog: &'a Client,
    cmt: bool,
    sp: String,
    /// The change is made on trial first.
    pub trial: bool,
}

impl<'a> Change<'a> {
    pub async fn start(client: &'a Client, catalog: &'a Client, cmt: bool) -> Result<Change<'a>> {
        let sp = savepoint();
        take(client).await?;
        client
            .batch_execute(&format!(
                "SAVEPOINT {sp}; SELECT set_config('as4tainr.pftrg_probe', '1', true)"
            ))
            .await?;
        Ok(Change {
            client,
            catalog,
            cmt,
            sp,
            trial: false,
        })
    }

    /// The change ended because it has *BEFORE trigger programs: it is
    /// taken back, to be made on trial.
    pub async fn probed<T>(&mut self, r: &Result<T>) -> bool {
        let hit = !self.trial && matches!(r, Err(e) if format!("{e:#}").contains(PROBE));
        if hit {
            let _ = self
                .client
                .batch_execute(&format!(
                    "ROLLBACK TO SAVEPOINT {}; SELECT set_config('as4tainr.pftrg_trial', '1', true)",
                    self.sp
                ))
                .await;
            let _ = take(self.client).await;
            self.trial = true;
        }
        hit
    }

    async fn undo(&self, journaled: bool) {
        if journaled {
            let _ = crate::journal::rollback_to(self.client, &self.sp).await;
        } else {
            let _ = self
                .client
                .batch_execute(&format!("ROLLBACK TO SAVEPOINT {}", self.sp))
                .await;
        }
        let _ = take(self.client).await;
    }

    /// A trigger program ended in error: the result, and whether to keep
    /// the change.
    pub async fn failed<T>(&self, err: anyhow::Error) -> (Result<T>, bool) {
        if self.cmt {
            self.undo(true).await;
        }
        let _ = self
            .client
            .batch_execute(
                "SELECT set_config('as4tainr.pftrg_skip_before', '0', true), set_config('as4tainr.pftrg_probe', '0', true),
                        set_config('as4tainr.pftrg_trial', '0', true)",
            )
            .await;
        let _ = self
            .client
            .batch_execute(&format!("RELEASE SAVEPOINT {}", self.sp))
            .await;
        (Err(err), !self.cmt)
    }

    /// The change was made on trial (`ok`: without an error); it is taken
    /// back. Its *BEFORE trigger program calls, a group for each record.
    pub async fn trial_made(&mut self, ok: bool) -> Result<Vec<Vec<Event>>> {
        let _ = self
            .client
            .batch_execute("SELECT set_config('as4tainr.pftrg_trial', '0', true)")
            .await;
        let events = take(self.client).await;
        self.undo(false).await;
        if !ok {
            let _ = self
                .client
                .batch_execute(&format!("RELEASE SAVEPOINT {}", self.sp))
                .await;
            return Ok(Vec::new());
        }
        let before: Vec<Event> = events?.into_iter().filter(|e| e.time == "2").collect();
        let mut groups: Vec<Vec<Event>> = Vec::new();
        for e in before {
            match groups.last_mut() {
                Some(g)
                    if g[0].lib == e.lib
                        && g[0].file == e.file
                        && g[0].rrn == e.rrn
                        && g[0].event == e.event =>
                {
                    g.push(e)
                }
                _ => groups.push(vec![e]),
            }
        }
        Ok(groups)
    }

    /// A record's *BEFORE programs ran: it is written with `new` when one
    /// changed it (`replaced`); an inserted record keeps its relative
    /// record number.
    pub async fn redo(
        &self,
        first: &Event,
        new: &Option<(Vec<u8>, String)>,
        replaced: bool,
    ) -> Result<()> {
        let op = match first.event.as_str() {
            "1" => "INSERT",
            "2" => "DELETE",
            _ => "UPDATE",
        };
        let row = match (new, replaced) {
            (Some((img, nulls)), true) => Some(
                crate::journal::image_json(self.catalog, &first.lib, &first.file, img, nulls)
                    .await?,
            ),
            _ => None,
        };
        self.client
            .execute(
                "SELECT as4tainr_svc.pftrg_redo($1, $2, $3, $4, $5::text::jsonb)",
                &[&first.lib, &first.file, &op, &first.rrn, &row],
            )
            .await?;
        Ok(())
    }

    /// The *BEFORE programs ran: the change is to be made again.
    pub async fn again(&self) -> Result<()> {
        self.client
            .batch_execute("SELECT set_config('as4tainr.pftrg_skip_before', '1', true)")
            .await?;
        Ok(())
    }

    /// The change was made (`ok`: without an error; otherwise it is taken
    /// back). Its *AFTER trigger program calls.
    pub async fn made(&self, ok: bool) -> Result<Vec<Event>> {
        let _ = self
            .client
            .batch_execute(
                "SELECT set_config('as4tainr.pftrg_skip_before', '0', true), set_config('as4tainr.pftrg_probe', '0', true),
                        as4tainr_svc.pftrg_redo(NULL, NULL, NULL, NULL, NULL)",
            )
            .await;
        if !ok {
            self.undo(self.cmt).await;
            let _ = self
                .client
                .batch_execute(&format!("RELEASE SAVEPOINT {}", self.sp))
                .await;
            return Ok(Vec::new());
        }
        Ok(take(self.client)
            .await?
            .into_iter()
            .filter(|e| e.time != "2")
            .collect())
    }

    /// The trigger programs ended normally.
    pub async fn done(&self) -> Result<()> {
        self.client
            .batch_execute(&format!("RELEASE SAVEPOINT {}", self.sp))
            .await?;
        Ok(())
    }
}

/// Make a change (`exec`, in the open transaction of `data`) with its
/// trigger programs ([`Change`]): the result, and whether to keep the
/// change though it is an error.
pub async fn run<T, F, Fut>(
    data: crate::runtime::DataSession,
    catalog: &Client,
    who: &Caller<'_>,
    cmt: bool,
    mut exec: F,
) -> (Result<T>, bool)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let client = data.client();
    let mut ch = match Change::start(client, catalog, cmt).await {
        Ok(c) => c,
        Err(e) => return (Err(e), false),
    };
    let mut r = exec().await;
    if ch.probed(&r).await {
        r = exec().await;
        let groups = match ch.trial_made(r.is_ok()).await {
            Ok(g) => g,
            Err(e) => return ch.failed(e).await,
        };
        if r.is_err() {
            return (r, false);
        }
        for g in groups {
            let mut new = g[0].new.clone();
            let mut replaced = false;
            for e in &g {
                let mut e = e.clone();
                e.new = new.clone();
                match call(&e, catalog, who, Some(data.clone())).await {
                    Ok(buf) => {
                        if let Some(img) = changed_new_image(&e, &buf) {
                            let nulls = new.as_ref().map(|n| n.1.clone()).unwrap_or_default();
                            new = Some((img, nulls));
                            replaced = true;
                        }
                    }
                    Err(err) => return ch.failed(err).await,
                }
            }
            if let Err(err) = ch.redo(&g[0], &new, replaced).await {
                return ch.failed(err).await;
            }
        }
        if let Err(err) = ch.again().await {
            return ch.failed(err).await;
        }
        r = exec().await;
    }
    let after = match ch.made(r.is_ok()).await {
        Ok(a) => a,
        Err(e) => return ch.failed(e).await,
    };
    if r.is_err() {
        return (r, false);
    }
    for e in &after {
        if let Err(err) = call(e, catalog, who, Some(data.clone())).await {
            return ch.failed(err).await;
        }
    }
    let _ = ch.done().await;
    (r, false)
}

/// The PostgreSQL triggers that note changes, on a file with trigger
/// programs; dropped when it has none left.
async fn sync_capture(client: &Client, lib: &str, file: &str) -> Result<()> {
    let any: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM as4tainr.file_triggers WHERE library=$1 AND file=$2)",
            &[&lib, &file],
        )
        .await?
        .get(0);
    let table = format!("{}.{}", ident(lib), ident(file));
    client
        .batch_execute(&format!(
            "DROP TRIGGER IF EXISTS \"as4tainr~pftrg_b\" ON {table};
             DROP TRIGGER IF EXISTS \"as4tainr~pftrg_a\" ON {table};"
        ))
        .await?;
    if any {
        let q = |s: &str| crate::dds::quote(s);
        client
            .batch_execute(&format!(
                "CREATE TRIGGER \"as4tainr~pftrg_b\" BEFORE INSERT OR UPDATE OR DELETE ON {table}
                   FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.pftrg_capture('B', {l}, {f});
                 CREATE TRIGGER \"as4tainr~pftrg_a\" AFTER INSERT OR UPDATE OR DELETE ON {table}
                   FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.pftrg_capture('A', {l}, {f});",
                l = q(lib),
                f = q(file)
            ))
            .await?;
    }
    Ok(())
}

impl Job<'_> {
    pub(crate) async fn pftrg_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        let msg = match cmd.def.name {
            "ADDPFTRG" => self.addpftrg(cmd).await?,
            "RMVPFTRG" => self.rmvpftrg(cmd).await?,
            _ => return Ok(None),
        };
        Ok(Some(Outcome::Messages(msg)))
    }

    /// The physical file a FILE parameter names.
    async fn trigger_file(&self, q: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(&q.to_ascii_uppercase());
        for l in &libs {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[l, &name],
                )
                .await?
            {
                let attr: String = r.get(0);
                if attr != "PF" {
                    bail!("CPF9898: File {name} in {l} is not a physical file.");
                }
                return Ok((l.clone(), name));
            }
        }
        let lib = q.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        bail!("CPF9812: File {name} in library {lib} not found.");
    }

    /// ADDPFTRG FILE TRGTIME TRGEVENT PGM [RPLTRG] [TRG] [TRGLIB]
    /// [ALWREPCHG] [TRGUPDCND].
    async fn addpftrg(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, file) = self.trigger_file(cmd.get("FILE")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &lib,
            &file,
            "*FILE",
            crate::authority::Aut::All,
        )
        .await?;
        let time = cmd.get("TRGTIME").to_ascii_uppercase();
        let event = cmd.get("TRGEVENT").to_ascii_uppercase();
        if !matches!(time.as_str(), "*BEFORE" | "*AFTER") {
            bail!("CPF0001: TRGTIME must be *BEFORE or *AFTER.");
        }
        if event == "*READ" {
            bail!("CPF9898: TRGEVENT(*READ) trigger programs are not supported yet.");
        }
        if !matches!(event.as_str(), "*INSERT" | "*DELETE" | "*UPDATE") {
            bail!("CPF0001: TRGEVENT must be *INSERT, *DELETE, *UPDATE or *READ.");
        }
        let (plibs, pgm) = self.search(&cmd.get("PGM").to_ascii_uppercase());
        let p = catalog::find_program(self.client, &plibs, &pgm)
            .await?
            .ok_or_else(|| anyhow!("CPF9801: Object {pgm} type *PGM not found."))?;
        let trg_lib = match cmd.get("TRGLIB").to_ascii_uppercase().as_str() {
            "" | "*FILE" => lib.clone(),
            "*CURLIB" => self.current_library(),
            l => l.to_string(),
        };
        let trg = match cmd
            .get("TRG")
            .trim_matches('\'')
            .to_ascii_uppercase()
            .as_str()
        {
            "" | "*GEN" => format!(
                "{file}_{}_{}",
                time.trim_start_matches('*'),
                event.trim_start_matches('*')
            ),
            t => t.to_string(),
        };
        let replace = cmd.get("RPLTRG").eq_ignore_ascii_case("*YES");
        // RPLTRG(*YES) replaces a trigger of the same name, or of the same
        // time and event.
        let existing: Vec<String> = self
            .client
            .query(
                "SELECT trg FROM as4tainr.file_triggers WHERE library=$1 AND file=$2
                   AND (trg=$3 OR (time=$4 AND event=$5))",
                &[&lib, &file, &trg, &time, &event],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        if !existing.is_empty() {
            if !replace {
                bail!(
                    "CPF9898: Trigger {} already exists for file {file} in {lib}.",
                    existing[0]
                );
            }
            self.client
                .execute(
                    "DELETE FROM as4tainr.file_triggers WHERE library=$1 AND file=$2 AND trg = ANY($3)",
                    &[&lib, &file, &existing],
                )
                .await?;
        }
        self.client
            .execute(
                "INSERT INTO as4tainr.file_triggers (library, file, trg_lib, trg, time, event, pgm_lib, pgm, alwrepchg, updcnd)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &lib,
                    &file,
                    &trg_lib,
                    &trg,
                    &time,
                    &event,
                    &p.library,
                    &p.name,
                    &cmd.get("ALWREPCHG").eq_ignore_ascii_case("*YES"),
                    &if cmd.get("TRGUPDCND").eq_ignore_ascii_case("*CHANGE") {
                        "*CHANGE"
                    } else {
                        "*ALWAYS"
                    },
                ],
            )
            .await?;
        sync_capture(self.client, &lib, &file).await?;
        Ok(vec![format!(
            "Trigger {trg} added to file {file} in {lib}: program {}/{} {time} {event}.",
            p.library, p.name
        )])
    }

    /// RMVPFTRG FILE [TRGTIME(*ALL)] [TRGEVENT(*ALL)] [TRG(*ALL)].
    async fn rmvpftrg(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, file) = self.trigger_file(cmd.get("FILE")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &lib,
            &file,
            "*FILE",
            crate::authority::Aut::All,
        )
        .await?;
        let up = |k: &str| {
            let v = cmd.get(k).trim_matches('\'').to_ascii_uppercase();
            if v.is_empty() {
                "*ALL".to_string()
            } else {
                v
            }
        };
        let (time, event, trg) = (up("TRGTIME"), up("TRGEVENT"), up("TRG"));
        let n = self
            .client
            .execute(
                "DELETE FROM as4tainr.file_triggers WHERE library=$1 AND file=$2
                   AND ($3 = '*ALL' OR time = $3) AND ($4 = '*ALL' OR event = $4) AND ($5 = '*ALL' OR trg = $5)",
                &[&lib, &file, &time, &event, &trg],
            )
            .await?;
        sync_capture(self.client, &lib, &file).await?;
        Ok(vec![format!(
            "{n} triggers removed from file {file} in {lib}."
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_buffer_layout() {
        // Db2 for i "Trigger buffer sections": names at 0, 10, 20; event
        // at 30, time at 31, commit lock level at 32; CCSID at 36; the
        // relative record number at 40; offsets and lengths of the
        // original record (48, 52), its null map (56, 60), the new record
        // (64, 68), its null map (72, 76).
        let e = Event {
            pgm_lib: "L".into(),
            pgm: "P".into(),
            time: "1".into(),
            event: "3".into(),
            lib: "COMLIB".into(),
            file: "ORDHDR".into(),
            rrn: 7,
            cmt: "0".into(),
            old: Some((vec![1, 2, 3], "00".into())),
            new: Some((vec![4, 5, 6], "01".into())),
            alwrepchg: false,
        };
        let b = buffer(&e);
        let i = |o: usize| i32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(crate::tn5250::ebcdic::decode(&b[0..10]), "ORDHDR    ");
        assert_eq!(crate::tn5250::ebcdic::decode(&b[10..20]), "COMLIB    ");
        assert_eq!(crate::tn5250::ebcdic::decode(&b[30..33]), "310");
        assert_eq!(i(36), 37);
        assert_eq!(i(40), 7);
        assert_eq!((i(48), i(52)), (96, 3));
        assert_eq!(&b[96..99], &[1, 2, 3]);
        assert_eq!((i(56), i(60)), (99, 2));
        assert_eq!((i(64), i(68)), (101, 3));
        assert_eq!(&b[101..104], &[4, 5, 6]);
        assert_eq!((i(72), i(76)), (104, 2));
        assert_eq!(crate::tn5250::ebcdic::decode(&b[104..106]), "01");
    }
}
