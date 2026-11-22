use super::{format, io, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const LEGACY_REVIEW: &str = "{\"review_required\":true}";
const REPORT: &str = "{\"report_requested\":true}";
pub enum Delivery {
    Current { first_stop: bool, confirmed: bool },
    Replay,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Stage {
    Accepted,
    Conflict,
    Unavailable,
}
enum Capture {
    Prompt(Option<String>),
    Report,
}
pub struct Claim {
    pub key: String,
    nonce: String,
    pub turn: String,
    pub data: Value,
}
pub struct Pending {
    state: PathBuf,
    cwd: PathBuf,
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn token_parts(token: &str) -> Option<(&str, &str)> {
    let (k, n) = token.split_once('.')?;
    (format::matches(r"\A[0-9a-f]{64}\z", k) && format::matches(r"\A[0-9a-f]{32}\z", n))
        .then_some((k, n))
}
pub use io::digest;
impl Pending {
    pub fn new(state: &Path, cwd: &Path) -> Self {
        Self {
            state: state.into(),
            cwd: cwd.into(),
        }
    }
    pub fn session_epoch(
        &self,
        session: &str,
        legacy: Option<String>,
        create: bool,
    ) -> Result<Option<String>> {
        let dir = self.state.join("memory-sessions");
        let path = dir.join(digest(&[session]));
        if let Some(epoch) = io::read(&path)? {
            return Ok(Some(epoch));
        }
        let Some(epoch) = legacy.filter(|e| !e.starts_with("coordinator-")) else {
            return Ok(Some(digest(&["memory-session", session])));
        };
        if !create {
            return Ok(Some(epoch));
        }
        io::state(&self.state, &self.cwd, true)?;
        io::directory(&dir, true, true)?;
        io::write(&path, &epoch, false, None)?;
        io::read(&path)
    }
    fn available(&self, create: bool) -> Result<bool> {
        if !io::state(&self.state, &self.cwd, create)? {
            return Ok(false);
        }
        let path = self.state.join("pending.sqlite3");
        if create && io::info(&path)?.is_none() {
            match io::options().write(true).create_new(true).open(&path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io::err(e)),
            }
        }
        let Some(m) = io::info(&path)? else {
            return Ok(false);
        };
        if !m.is_file() {
            return Err("Unsafe pending memory database".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if m.nlink() != 1 || m.uid() != unsafe { libc::getuid() } || m.mode() & 0o077 != 0 {
                return Err("Unsafe pending memory database".into());
            }
        }
        #[cfg(windows)]
        {
            if io::windows_info(&path, false)?.2 != 1 {
                return Err("Unsafe pending memory database".into());
            }
        }
        if let Some(m) = io::info(&self.state.join("pending.sqlite3-journal"))? {
            if !m.is_file() {
                return Err("Unsafe pending memory journal".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if m.nlink() != 1 {
                    return Err("Unsafe pending memory journal".into());
                }
            }
        }
        #[cfg(windows)]
        if io::windows_info_if_present(&self.state.join("pending.sqlite3-journal"), false)?
            .is_some_and(|(_, _, links)| links != 1)
        {
            return Err("Unsafe pending memory journal".into());
        }
        for name in ["pending.sqlite3-wal", "pending.sqlite3-shm"] {
            if io::info(&self.state.join(name))?.is_some() {
                return Err("Unexpected pending memory database files".into());
            }
        }
        Ok(true)
    }
    fn connection(&self, create: bool, readonly: bool) -> Result<Option<Connection>> {
        if !self.available(create)? {
            return Ok(None);
        }
        let flags = if readonly {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let conn = Connection::open_with_flags(self.state.join("pending.sqlite3"), flags)
            .map_err(io::err)?;
        conn.busy_timeout(Duration::from_secs(1)).map_err(io::err)?;
        if !readonly {
            conn.execute_batch(
                "PRAGMA secure_delete=ON; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS pending (
                    turn TEXT PRIMARY KEY, nonce TEXT NOT NULL, scope TEXT NOT NULL,
                    expires INTEGER NOT NULL, action TEXT
                );
                CREATE TABLE IF NOT EXISTS issued_modes (
                    turn TEXT PRIMARY KEY, nonce TEXT NOT NULL, epoch TEXT NOT NULL, session TEXT NOT NULL, original_turn TEXT
                );
                CREATE TABLE IF NOT EXISTS ordinary_turns (
                    turn TEXT PRIMARY KEY, nonce TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS claimed (
                    turn TEXT PRIMARY KEY, nonce TEXT NOT NULL, scope TEXT NOT NULL,
                    expires INTEGER NOT NULL, action TEXT NOT NULL, session TEXT NOT NULL,
                    original_turn TEXT NOT NULL, epoch TEXT NOT NULL, attempted INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS diagnostics (
                    scope TEXT PRIMARY KEY, phase TEXT NOT NULL, updated INTEGER NOT NULL
                );",
            )
            .map_err(io::err)?;
            let has_turn: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_info('issued_modes') WHERE name='original_turn')",
                [], |row| row.get(0),
            ).map_err(io::err)?;
            if !has_turn {
                conn.execute("ALTER TABLE issued_modes ADD COLUMN original_turn TEXT", [])
                    .map_err(io::err)?;
            }
        }
        Ok(Some(conn))
    }
    fn generation(&self, key: &str) -> Result<Option<String>> {
        if !io::state(&self.state, &self.cwd, false)? {
            return Ok(None);
        }
        io::read(&self.state.join("pending-generations").join(key))
    }
    fn current(&self, key: &str, nonce: &str) -> Result<bool> {
        if token_parts(&format!("{key}.{nonce}")).is_none() {
            return Err("Invalid pending memory generation".into());
        }
        let revoked = self.state.join("pending-revocations");
        Ok(self.generation(key)?.as_deref() == Some(nonce)
            && !(io::directory(&revoked, false, false)?
                && io::info(&revoked.join(format!("{key}.{nonce}")))?.is_some()))
    }
    fn lock_markers(&self) -> Result<std::fs::File> {
        let key = digest(&[&io::normalized(&self.state)]);
        io::lock_key(&format!("pending-markers-{}", &key[..2]), None)
    }
    fn prune_markers(&self) -> Result<()> {
        let sweep = self.state.join("pending-marker-sweep");
        let now = SystemTime::now();
        let older_than = |metadata: std::fs::Metadata, seconds| -> Result<bool> {
            Ok(now
                .duration_since(metadata.modified().map_err(io::err)?)
                .unwrap_or_default()
                > Duration::from_secs(seconds))
        };
        if let Some(metadata) = io::info(&sweep)? {
            if !older_than(metadata, 60)? {
                return Ok(());
            }
        }
        for (folder, tokens) in [
            ("pending-generations", false),
            ("pending-revocations", true),
        ] {
            let dir = self.state.join(folder);
            if !io::directory(&dir, false, false)? {
                continue;
            }
            for entry in std::fs::read_dir(&dir).map_err(io::err)? {
                let entry = entry.map_err(io::err)?;
                let name = entry.file_name();
                let valid = name.to_str().is_some_and(|name| {
                    if tokens {
                        token_parts(name).is_some()
                    } else {
                        format::id(name).is_ok()
                    }
                });
                if valid
                    && entry.file_type().map_err(io::err)?.is_file()
                    && older_than(entry.metadata().map_err(io::err)?, 7200)?
                {
                    io::delete(&entry.path())?;
                }
            }
        }
        io::write(&sweep, "", true, None)?;
        Ok(())
    }
    pub fn invalidate(&self, key: &str) -> Result<Option<String>> {
        if io::info(&self.state.join("pending-generations").join(key))?.is_none() {
            return Ok(None);
        }
        let _turn_lock = self.lock_turn(key)?;
        let _markers = self.lock_markers()?;
        let Some(previous) = self.generation(key)? else {
            return Ok(None);
        };
        if token_parts(&format!("{key}.{previous}")).is_none() {
            return Err("Cannot revoke pending memory action".into());
        }
        let dir = self.state.join("pending-revocations");
        let marker = dir.join(format!("{key}.{previous}"));
        if !io::directory(&dir, false, false)? || io::info(&marker)?.is_none() {
            io::directory(&dir, true, true)?;
            io::write(&marker, "", false, None)?;
            if io::info(&marker)?.is_none() {
                return Err("Missing pending memory revocation marker".into());
            }
        }
        let nonce = io::nonce()?;
        io::write(
            &self.state.join("pending-generations").join(key),
            &nonce,
            true,
            None,
        )?;
        Ok(Some(nonce))
    }
    pub fn cancel(&self, key: &str) -> Result<()> {
        if self.invalidate(key)?.is_none() {
            return Ok(());
        }
        let Some(conn) = self.connection(false, false)? else {
            return Ok(());
        };
        let scope: Option<String> = conn.query_row(
            "SELECT scope FROM pending WHERE turn=?1 UNION SELECT scope FROM claimed WHERE turn=?1",
            [key], |row| row.get(0),
        ).optional().map_err(io::err)?;
        if let Some(scope) = scope {
            Self::phase(&conn, &scope, "cancelled")?;
        }
        conn.execute_batch("COMMIT").map_err(io::err)
    }
    pub fn lock_turn(&self, key: &str) -> Result<std::fs::File> {
        format::id(key)?;
        io::lock_key(
            &digest(&["pending-turn", &io::normalized(&self.state), &key[..2]]),
            None,
        )
    }
    pub fn issue(
        &self,
        key: &str,
        scope: &str,
        generation: Option<String>,
        epoch: &str,
        session: &str,
        turn: &str,
    ) -> Result<String> {
        self.capture(
            key,
            scope,
            Capture::Prompt(generation),
            epoch,
            session,
            turn,
        )
    }
    pub fn has_generation(&self, key: &str) -> Result<bool> {
        Ok(self.generation(key)?.is_some())
    }
    pub fn request_report(
        &self,
        key: &str,
        scope: &str,
        epoch: &str,
        session: &str,
        turn: &str,
    ) -> Result<bool> {
        let _guard = self.lock_turn(key)?;
        if self.has_generation(key)? {
            return Ok(false);
        }
        self.capture(key, scope, Capture::Report, epoch, session, turn)?;
        Ok(true)
    }
    fn capture(
        &self,
        key: &str,
        scope: &str,
        capture: Capture,
        epoch: &str,
        session: &str,
        turn: &str,
    ) -> Result<String> {
        let (generation, action) = match capture {
            Capture::Prompt(generation) => (generation, None),
            Capture::Report => (None, Some(REPORT)),
        };
        let nonce = match generation {
            Some(n) => n,
            None => {
                io::state(&self.state, &self.cwd, true)?;
                let dir = self.state.join("pending-generations");
                io::directory(&dir, true, true)?;
                let _markers = self.lock_markers()?;
                self.prune_markers()?;
                let n = io::nonce()?;
                if !io::write(&dir.join(key), &n, false, None)? {
                    return Err("Memory turn was replaced by a newer input".into());
                }
                n
            }
        };
        let conn = self
            .connection(true, false)?
            .ok_or("Cannot open pending memory database")?;
        if !self.current(key, &nonce)? {
            return Err("Memory turn was replaced by a newer input".into());
        }
        conn.execute("DELETE FROM pending WHERE expires<?", [now()])
            .map_err(io::err)?;
        conn.execute(
            "DELETE FROM issued_modes WHERE turn NOT IN (SELECT turn FROM pending)",
            [],
        )
        .map_err(io::err)?;
        conn.execute(
            "DELETE FROM claimed WHERE expires<? OR turn=?",
            params![now(), key],
        )
        .map_err(io::err)?;
        let count: i64 = conn
            .query_row(
                "SELECT (SELECT count(*) FROM pending)+(SELECT count(*) FROM claimed)",
                [],
                |r| r.get(0),
            )
            .map_err(io::err)?;
        if count >= 256 {
            return Err("Pending memory turn limit exceeded".into());
        }
        conn.execute("DELETE FROM ordinary_turns WHERE NOT EXISTS (SELECT 1 FROM pending p WHERE p.turn=ordinary_turns.turn AND p.nonce=ordinary_turns.nonce)", []).map_err(io::err)?;
        conn.execute("INSERT OR REPLACE INTO ordinary_turns SELECT turn,nonce FROM issued_modes WHERE turn=? AND nonce=? AND epoch='setup' AND ?<>'setup'",params![key,nonce,epoch]).map_err(io::err)?;
        conn.execute("INSERT INTO pending (turn,nonce,scope,expires,action) VALUES (?,?,?,?,?) ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce,scope=excluded.scope,expires=excluded.expires,action=excluded.action",params![key,nonce,scope,now()+3600,action]).map_err(io::err)?;
        conn.execute("INSERT INTO issued_modes VALUES (?,?,?,?,?) ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce,epoch=excluded.epoch,session=excluded.session,original_turn=excluded.original_turn", params![key, nonce, epoch, session, turn]).map_err(io::err)?;
        Self::phase(
            &conn,
            scope,
            if action.is_some() {
                "review"
            } else {
                "capture"
            },
        )?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(format!("{key}.{nonce}"))
    }
    pub fn active(&self, key: &str) -> Result<bool> {
        let Some(conn) = self.connection(false, true)? else {
            return Ok(false);
        };
        let nonce: Option<String> = conn
            .query_row(
                "SELECT nonce FROM pending WHERE turn=? AND expires>=?",
                params![key, now()],
                |row| row.get(0),
            )
            .optional()
            .map_err(io::err)?;
        nonce
            .map(|nonce| self.current(key, &nonce))
            .unwrap_or(Ok(false))
    }
    pub fn is_current(&self, token: &str, scope: &str) -> Result<bool> {
        let Some((key, nonce)) = token_parts(token) else {
            return Ok(false);
        };
        if !self.current(key, nonce)? {
            return Ok(false);
        }
        let Some(conn) = self.connection(false, true)? else {
            return Ok(false);
        };
        let found = conn
            .query_row(
                "SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=?",
                params![key, nonce, scope, now()],
                |_| Ok(()),
            )
            .optional()
            .map_err(io::err)?
            .is_some();
        Ok(found && self.current(key, nonce)?)
    }
    pub fn confirmed(&self, key: &str, scope: &str, epoch: &str) -> Result<bool> {
        let Some(nonce) = self.generation(key)? else {
            return Ok(false);
        };
        if !self.current(key, &nonce)? {
            return Ok(false);
        }
        let Some(conn) = self.connection(false, false)? else {
            return Ok(false);
        };
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending WHERE turn=?1 AND nonce=?2 AND scope=?3 AND expires>=?4 AND (action NOT IN (?5,?6) OR EXISTS(SELECT 1 FROM ordinary_turns o WHERE o.turn=pending.turn AND o.nonce=pending.nonce)))
              OR EXISTS(SELECT 1 FROM claimed WHERE turn=?1 AND nonce=?2 AND scope=?3 AND expires>=?4 AND epoch=?7)",
            params![key, nonce, scope, now(), LEGACY_REVIEW, REPORT, epoch], |row| row.get(0)
        ).map_err(io::err)
    }
    pub fn stage(&self, key: &str, token: &str, scope: &str, action: &Value) -> Result<Stage> {
        let Some((k, nonce)) = token_parts(token) else {
            return Ok(Stage::Unavailable);
        };
        if k != key {
            return Ok(Stage::Unavailable);
        }
        let _turn_lock = self.lock_turn(key)?;
        if !self.current(key, nonce)? {
            return Ok(Stage::Unavailable);
        }
        let Some(conn) = self.connection(false, false)? else {
            return Ok(Stage::Unavailable);
        };
        if !self.current(key, nonce)? {
            return Ok(Stage::Unavailable);
        }
        let serialized = action.to_string();
        let changed=conn.execute("UPDATE pending SET action=? WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND (action IS NULL OR action=? OR action=?)",params![serialized,key,nonce,scope,now(),serialized,LEGACY_REVIEW]).map_err(io::err)?;
        let conflict = if changed == 0 {
            conn.query_row("SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND action<>?",params![key,nonce,scope,now(),serialized],|_|Ok(())).optional().map_err(io::err)?.is_some()&&self.current(key,nonce)?
        } else {
            false
        };
        if changed != 0 {
            Self::phase(&conn, scope, "stage")?;
        }
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(if changed != 0 {
            Stage::Accepted
        } else if conflict {
            Stage::Conflict
        } else {
            Stage::Unavailable
        })
    }
    pub fn accept_report(&self, key: &str, scope: &str, action: &Value) -> Result<bool> {
        let Some(nonce) = self.generation(key)? else {
            return Ok(false);
        };
        let Some(conn) = self.connection(false, false)? else {
            return Ok(false);
        };
        if !self.current(key, &nonce)? {
            return Ok(false);
        }
        let changed = conn.execute(
            "UPDATE pending SET action=? WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND action=?",
            params![json!({"report":action}).to_string(), key, nonce, scope, now(), REPORT],
        ).map_err(io::err)?;
        if changed == 0 {
            return Ok(false);
        }
        Self::phase(&conn, scope, "stage")?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(true)
    }
    pub fn claim(
        &self,
        key: &str,
        scope: &str,
        session: &str,
        turn: &str,
        epoch: &str,
        delivery: Delivery,
    ) -> Result<bool> {
        let _turn_lock = self.lock_turn(key)?;
        let Some(nonce) = self.generation(key)? else {
            return Ok(false);
        };
        let Some(conn) = self.connection(false, false)? else {
            return Ok(false);
        };
        if !self.current(key, &nonce)? {
            return Ok(false);
        }
        let replay = matches!(delivery, Delivery::Replay);
        if digest(&[session, turn]) != key {
            return Err("Invalid memory turn identity".into());
        }
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT p.action FROM pending p
             LEFT JOIN issued_modes m ON m.turn=p.turn AND m.nonce=p.nonce
             WHERE p.turn=? AND p.nonce=? AND p.scope=? AND p.expires>=? AND COALESCE(m.epoch,'')=?
             AND (NOT ? OR (m.session=? AND m.original_turn=? AND p.action NOT IN (?,?)))",
                params![
                    key,
                    nonce,
                    scope,
                    now(),
                    epoch,
                    replay,
                    session,
                    turn,
                    LEGACY_REVIEW,
                    REPORT
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(io::err)?;
        let Some(action) = row else {
            return Ok(false);
        };
        if action.as_deref() == Some(REPORT)
            && matches!(
                delivery,
                Delivery::Current {
                    first_stop: true,
                    ..
                }
            )
        {
            return Ok(false);
        }
        let claimed = action
            .as_deref()
            .is_some_and(|s| !matches!(s, LEGACY_REVIEW | REPORT));
        if claimed {
            conn.execute(
                "INSERT INTO claimed SELECT turn,nonce,scope,expires,action,?,?,?,0 FROM pending WHERE turn=?",
                params![session,turn,epoch,key],
            ).map_err(io::err)?;
            Self::phase(&conn, scope, "pending")?;
        } else {
            let phase = match (action.as_deref(), delivery) {
                (
                    None | Some(LEGACY_REVIEW),
                    Delivery::Current {
                        confirmed: true, ..
                    },
                ) => "skipped",
                _ => "unconfirmed",
            };
            Self::phase(&conn, scope, phase)?;
        }
        conn.execute("DELETE FROM pending WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute("DELETE FROM ordinary_turns WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute("DELETE FROM issued_modes WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(claimed)
    }

    pub fn due(
        &self,
        scope: &str,
        session: &str,
        epoch: &str,
        key: &str,
    ) -> Result<Vec<(String, String)>> {
        let Some(conn) = self.connection(false, false)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn
            .prepare(
                "SELECT turn,nonce,original_turn,expires,attempted,(turn=?) AS current
             FROM claimed WHERE scope=? AND session=? AND epoch=? AND expires>=?
             UNION ALL
             SELECT p.turn,p.nonce,m.original_turn,p.expires,0,0
             FROM pending p JOIN issued_modes m ON m.turn=p.turn AND m.nonce=p.nonce
             WHERE p.scope=? AND m.session=? AND m.epoch=? AND p.expires>=? AND p.turn<>?
             AND m.original_turn IS NOT NULL AND p.action NOT IN (?,?)
             ORDER BY current DESC, attempted, expires, turn",
            )
            .map_err(io::err)?;
        let rows = stmt
            .query_map(
                params![
                    key,
                    scope,
                    session,
                    epoch,
                    now(),
                    scope,
                    session,
                    epoch,
                    now(),
                    key,
                    LEGACY_REVIEW,
                    REPORT
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(io::err)?;
        let mut due = Vec::new();
        for row in rows {
            let (key, nonce, turn) = row.map_err(io::err)?;
            if self.current(&key, &nonce)? {
                due.push((key, turn));
                if due.len() == 2 {
                    break;
                }
            }
        }
        Ok(due)
    }

    pub fn operation(
        &self,
        key: &str,
        scope: &str,
        session: &str,
        epoch: &str,
    ) -> Result<Option<Claim>> {
        let Some(conn) = self.connection(false, false)? else {
            return Ok(None);
        };
        let row: Option<(String, String, String)> = conn.query_row(
            "SELECT nonce,original_turn,action FROM claimed WHERE turn=? AND scope=? AND session=? AND epoch=? AND expires>=?",
            params![key,scope,session,epoch,now()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional().map_err(io::err)?;
        let Some((nonce, turn, action)) = row else {
            return Ok(None);
        };
        if !self.current(key, &nonce)? {
            conn.execute(
                "DELETE FROM claimed WHERE turn=? AND nonce=?",
                params![key, nonce],
            )
            .map_err(io::err)?;
            conn.execute_batch("COMMIT").map_err(io::err)?;
            return Ok(None);
        }
        Ok(Some(Claim {
            key: key.into(),
            nonce,
            turn,
            data: serde_json::from_str(&action).map_err(io::err)?,
        }))
    }

    pub fn finish(&self, scope: &str, claim: &Claim, phase: &str, complete: bool) -> Result<()> {
        let conn = self
            .connection(false, false)?
            .ok_or("Cannot open pending memory database")?;
        if complete {
            conn.execute(
                "DELETE FROM claimed WHERE turn=? AND nonce=?",
                params![claim.key, claim.nonce],
            )
            .map_err(io::err)?;
        }
        if !complete {
            conn.execute(
                "UPDATE claimed SET attempted=? WHERE turn=? AND nonce=?",
                params![now(), claim.key, claim.nonce],
            )
            .map_err(io::err)?;
        }
        Self::phase(&conn, scope, phase)?;
        conn.execute_batch("COMMIT").map_err(io::err)
    }

    fn phase(conn: &Connection, scope: &str, phase: &str) -> Result<()> {
        conn.execute("DELETE FROM diagnostics WHERE updated<?", [now() - 3600])
            .map_err(io::err)?;
        conn.execute("DELETE FROM diagnostics WHERE scope IN (SELECT scope FROM diagnostics ORDER BY updated DESC,scope LIMIT -1 OFFSET 255)", []).map_err(io::err)?;
        conn.execute("INSERT INTO diagnostics VALUES (?,?,?) ON CONFLICT(scope) DO UPDATE SET phase=excluded.phase,updated=excluded.updated",params![scope,phase,now()]).map_err(io::err)?;
        Ok(())
    }

    pub fn diagnostics(&self, scope: &str) -> Result<Value> {
        let empty = json!({"phase":null,"staged":0,"pending":0});
        let Some(conn) = self.connection(false, true)? else {
            return Ok(empty);
        };
        let has_diagnostics: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name='diagnostics')",
                [],
                |row| row.get(0),
            )
            .map_err(io::err)?;
        if !has_diagnostics {
            return Ok(empty);
        }
        let phase: Option<String> = conn
            .query_row(
                "SELECT phase FROM diagnostics WHERE scope=? AND updated>=?",
                params![scope, now() - 3600],
                |row| row.get(0),
            )
            .optional()
            .map_err(io::err)?;
        let mut staged = 0;
        let mut queued = 0;
        let mut stmt = conn
            .prepare(
                "SELECT p.turn,p.nonce,0,m.session,m.epoch FROM pending p
             LEFT JOIN issued_modes m ON m.turn=p.turn AND m.nonce=p.nonce
             WHERE p.scope=? AND p.expires>=? AND p.action NOT IN (?,?)
             UNION ALL SELECT turn,nonce,1,session,epoch FROM claimed WHERE scope=? AND expires>=?",
            )
            .map_err(io::err)?;
        let rows = stmt
            .query_map(
                params![scope, now(), LEGACY_REVIEW, REPORT, scope, now()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(io::err)?;
        for row in rows {
            let (key, nonce, claimed, session, epoch) = row.map_err(io::err)?;
            if let Some(session) = session {
                let marker = self
                    .state
                    .join("faust-mode-sessions")
                    .join(digest(&[&session]));
                if self.session_epoch(&session, io::read(&marker)?, false)? != epoch {
                    continue;
                }
            }
            if self.current(&key, &nonce)? {
                if claimed {
                    queued += 1;
                } else {
                    staged += 1;
                }
            }
        }
        Ok(json!({"phase":phase,"staged":staged,"pending":queued}))
    }
}
