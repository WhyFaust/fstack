use super::{format, io, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub const REVIEW: &str = "{\"review_required\":true}";
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
        if io::info(&self.state.join("pending.sqlite3-journal"))?.is_some()
            && io::windows_info(&self.state.join("pending.sqlite3-journal"), false)?.2 != 1
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
            conn.execute_batch("PRAGMA secure_delete=ON; BEGIN IMMEDIATE; CREATE TABLE IF NOT EXISTS pending (turn TEXT PRIMARY KEY,nonce TEXT NOT NULL,scope TEXT NOT NULL,expires INTEGER NOT NULL,action TEXT)").map_err(io::err)?;
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
    pub fn issue(&self, key: &str, scope: &str, generation: Option<String>) -> Result<String> {
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
        let count: i64 = conn
            .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))
            .map_err(io::err)?;
        if count >= 256 {
            return Err("Pending memory turn limit exceeded".into());
        }
        conn.execute("INSERT INTO pending (turn,nonce,scope,expires) VALUES (?,?,?,?) ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce,scope=excluded.scope,expires=excluded.expires,action=NULL",params![key,nonce,scope,now()+3600]).map_err(io::err)?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(format!("{key}.{nonce}"))
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
        conn.execute_batch("COMMIT").map_err(io::err)?;
        Ok(conflict)
    }
    pub fn consume(&self, key: &str, scope: &str, review: bool) -> Result<Option<Value>> {
        let Some(nonce) = self.generation(key)? else {
            return Ok(None);
        };
        let Some(conn) = self.connection(false, false)? else {
            return Ok(None);
        };
        if self.generation(key)?.as_deref() != Some(&nonce) {
            return Ok(None);
        }
        let row: Option<Option<String>> = if self.current(key, &nonce)? {
            conn.query_row(
                "SELECT action FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=?",
                params![key, nonce, scope, now()],
                |r| r.get(0),
            )
            .optional()
            .map_err(io::err)?
        } else {
            None
        };
        if row == Some(None) && review && self.current(key, &nonce)? {
            conn.execute(
                "UPDATE pending SET action=? WHERE turn=?",
                params![REVIEW, key],
            )
            .map_err(io::err)?;
            conn.execute_batch("COMMIT").map_err(io::err)?;
            return Ok(Some(json!({"review_required":true})));
        }
        conn.execute("DELETE FROM pending WHERE turn=?", [key])
            .map_err(io::err)?;
        conn.execute_batch("COMMIT").map_err(io::err)?;
        if let Some(action) = row {
            if self.current(key, &nonce)? {
                return match action {
                    None => Ok(Some(json!({"review_required":false}))),
                    Some(s) if s == REVIEW => Ok(Some(json!({"review_required":false}))),
                    Some(s) => Ok(Some(serde_json::from_str(&s).map_err(io::err)?)),
                };
            }
        }
        Ok(None)
    }
}
