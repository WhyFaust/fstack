use super::{format, io, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub const REVIEW: &str = "{\"review_required\":true}";
#[derive(Debug, PartialEq)]
pub enum Stop {
    Review,
    Missing,
    Claimed,
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
fn ascii_json(text: &str) -> String {
    let mut out = String::new();
    for c in serde_json::to_string(text).unwrap().chars() {
        if (c as u32) < 127 {
            out.push(c)
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}
pub fn digest(parts: &[&str]) -> String {
    io::hash(format!(
        "[{}]",
        parts
            .iter()
            .map(|s| ascii_json(s))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
impl Pending {
    pub fn new(state: &Path, cwd: &Path) -> Self {
        Self {
            state: state.into(),
            cwd: cwd.into(),
        }
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
                    turn TEXT PRIMARY KEY, nonce TEXT NOT NULL, epoch TEXT NOT NULL, session TEXT NOT NULL
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
    fn prune(&self, dir: &Path, tokens: bool, limit: usize) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .map_err(io::err)?
            .take(limit + 1)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io::err)?;
        let mut remaining = entries.len();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(if tokens {
                token_parts(&name).is_some()
            } else {
                format::id(&name).is_ok()
            }) {
                continue;
            }
            let meta = entry.metadata().map_err(io::err)?;
            if !entry.file_type().map_err(io::err)?.is_file() {
                continue;
            }
            if SystemTime::now()
                .duration_since(meta.modified().map_err(io::err)?)
                .unwrap_or_default()
                > Duration::from_secs(3601)
                && io::delete(&entry.path())?
            {
                remaining -= 1
            }
        }
        if remaining >= limit {
            return Err("Pending memory marker limit exceeded".into());
        }
        Ok(())
    }
    pub fn invalidate(&self, key: &str) -> Result<Option<String>> {
        if !io::state(&self.state, &self.cwd, false)? {
            return Ok(None);
        }
        let _turn_lock = self.lock_turn(key)?;
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
            self.prune(&dir, true, 1024)?;
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
    ) -> Result<String> {
        let nonce = match generation {
            Some(n) => n,
            None => {
                io::state(&self.state, &self.cwd, true)?;
                let dir = self.state.join("pending-generations");
                io::directory(&dir, true, true)?;
                self.prune(&dir, false, 256)?;
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
        conn.execute("INSERT INTO pending (turn,nonce,scope,expires) VALUES (?,?,?,?) ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce,scope=excluded.scope,expires=excluded.expires,action=NULL",params![key,nonce,scope,now()+3600]).map_err(io::err)?;
        conn.execute("INSERT INTO issued_modes VALUES (?,?,?,?) ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce,epoch=excluded.epoch,session=excluded.session", params![key, nonce, epoch, session]).map_err(io::err)?;
        Self::phase(&conn, scope, "capture")?;
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
    pub fn stage(&self, key: &str, token: &str, scope: &str, action: &Value) -> Result<bool> {
        let Some((k, nonce)) = token_parts(token) else {
            return Ok(false);
        };
        if k != key || !self.current(key, nonce)? {
            return Ok(false);
        }
        let Some(conn) = self.connection(false, false)? else {
            return Ok(false);
        };
        if !self.current(key, nonce)? {
            return Ok(false);
        }
        let serialized = action.to_string();
        let changed=conn.execute("UPDATE pending SET action=? WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND (action IS NULL OR action=? OR action=?)",params![serialized,key,nonce,scope,now(),serialized,REVIEW]).map_err(io::err)?;
        let conflict = if changed == 0 {
            conn.query_row("SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND action<>?",params![key,nonce,scope,now(),serialized],|_|Ok(())).optional().map_err(io::err)?.is_some()&&self.current(key,nonce)?
        } else {
            false
        };
        if changed != 0 {
            Self::phase(&conn, scope, "stage")?;
        }
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(conflict)
    }
    pub fn claim(
        &self,
        key: &str,
        scope: &str,
        session: &str,
        turn: &str,
        epoch: &str,
        review: bool,
    ) -> Result<Option<Stop>> {
        let _turn_lock = self.lock_turn(key)?;
        let Some(nonce) = self.generation(key)? else {
            return Ok(None);
        };
        let Some(conn) = self.connection(false, false)? else {
            return Ok(None);
        };
        if !self.current(key, &nonce)? {
            return Ok(None);
        }
        let row: Option<Option<String>> = conn.query_row(
            "SELECT action FROM pending p WHERE turn=? AND nonce=? AND scope=? AND expires>=?
             AND COALESCE((SELECT m.epoch FROM issued_modes m WHERE m.turn=p.turn AND m.nonce=p.nonce), '')=?",
            params![key, nonce, scope, now(), epoch],
            |r| r.get(0),
        ).optional().map_err(io::err)?;
        let Some(action) = row else {
            return Ok(None);
        };
        if action.is_none() && review {
            conn.execute(
                "UPDATE pending SET action=? WHERE turn=?",
                params![REVIEW, key],
            )
            .map_err(io::err)?;
            Self::phase(&conn, scope, "review")?;
            conn.execute_batch("COMMIT").map_err(io::err)?;
            return Ok(Some(Stop::Review));
        }
        let outcome = if action.as_deref().is_some_and(|s| s != REVIEW) {
            conn.execute(
                "INSERT INTO claimed SELECT turn,nonce,scope,expires,action,?,?,?,0 FROM pending WHERE turn=?",
                params![session,turn,epoch,key],
            ).map_err(io::err)?;
            Self::phase(&conn, scope, "pending")?;
            Stop::Claimed
        } else {
            Self::phase(&conn, scope, "unconfirmed")?;
            Stop::Missing
        };
        conn.execute("DELETE FROM pending WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute("DELETE FROM issued_modes WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(Some(outcome))
    }

    pub fn due(&self, scope: &str, session: &str, epoch: &str, key: &str) -> Result<Vec<String>> {
        let Some(conn) = self.connection(false, false)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn
            .prepare(
                "SELECT turn FROM claimed WHERE scope=? AND session=? AND epoch=? AND expires>=?
             ORDER BY (turn=?) DESC, attempted, expires, turn LIMIT 2",
            )
            .map_err(io::err)?;
        let rows = stmt
            .query_map(params![scope, session, epoch, now(), key], |row| row.get(0))
            .map_err(io::err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io::err)
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
             WHERE p.scope=? AND p.expires>=? AND p.action IS NOT NULL AND p.action<>?
             UNION ALL SELECT turn,nonce,1,session,epoch FROM claimed WHERE scope=? AND expires>=?",
            )
            .map_err(io::err)?;
        let rows = stmt
            .query_map(params![scope, now(), REVIEW, scope, now()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(io::err)?;
        for row in rows {
            let (key, nonce, claimed, session, epoch) = row.map_err(io::err)?;
            if let Some(session) = session {
                let marker = self
                    .state
                    .join("faust-mode-sessions")
                    .join(digest(&[&session]));
                if io::read(&marker)? != epoch {
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
