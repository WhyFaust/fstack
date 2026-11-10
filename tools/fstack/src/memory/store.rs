use super::{
    format::{self as fmt, Fields, APPROVED, FORGOTTEN, MEMORY},
    io, scope, views, Result,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Binding {
    pub root: PathBuf,
    pub project: String,
    pub mode: String,
}
pub struct Store {
    pub state: PathBuf,
    pub package: PathBuf,
}
pub fn record(root: &Path, id: &str) -> PathBuf {
    root.join("Records").join(format!("{id}.md"))
}
fn captured(root: &Path, id: &str) -> PathBuf {
    root.join("Records").join(format!(".fstack-forget-{id}"))
}
fn tombstone(root: &Path, id: &str) -> PathBuf {
    root.join("Records").join(format!("{id}-forgotten.json"))
}
fn legacy(root: &Path, folder: &str, id: &str) -> PathBuf {
    root.join(folder).join(format!("{id}.md"))
}
pub fn note(path: &Path, id: &str, marker: &str, body: bool) -> Result<Option<Fields>> {
    io::read(path)?
        .map(|s| fmt::parse(&s, id, marker, body))
        .transpose()
}
pub fn files(dir: &Path, tombstones: bool) -> Result<Vec<PathBuf>> {
    if !io::directory(dir, false, false)? {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut count = 0;
    for entry in fs::read_dir(dir).map_err(io::err)? {
        let path = entry.map_err(io::err)?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        let is_tombstone = dir.file_name().is_some_and(|s| s == "Records")
            && name
                .strip_suffix("-forgotten.json")
                .is_some_and(|s| fmt::id(s).is_ok());
        if is_tombstone != tombstones {
            continue;
        }
        count += 1;
        if count > 4096 {
            return Err("Memory directory entry limit exceeded".into());
        }
        if tombstones
            || (path.extension().is_some_and(|s| s == "md")
                && path
                    .file_stem()
                    .is_some_and(|s| fmt::id(&s.to_string_lossy()).is_ok()))
        {
            result.push(path)
        }
    }
    Ok(result)
}
fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}
pub fn tombstone_ids(root: &Path) -> Result<BTreeSet<String>> {
    let mut ids = files(&root.join("Forgotten"), false)?
        .iter()
        .map(|p| stem(p))
        .collect::<BTreeSet<_>>();
    for p in files(&root.join("Records"), true)? {
        ids.insert(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches("-forgotten.json")
                .into(),
        );
    }
    if ids.len() > 4096 {
        return Err("Memory tombstone entry limit exceeded".into());
    }
    Ok(ids)
}
fn read_tombstones(
    root: &Path,
    id: &str,
) -> Result<(Option<String>, Option<Value>, Option<Fields>)> {
    let old = note(&legacy(root, "Forgotten", id), id, FORGOTTEN, true)?;
    let raw = io::read(&tombstone(root, id))?;
    let value = raw
        .as_ref()
        .map(|s| -> Result<Value> {
            let v: Value = serde_json::from_str(s).map_err(io::err)?;
            let m = v.as_object().ok_or("Invalid memory tombstone")?;
            if m.keys().any(|k| !matches!(k.as_str(), "id" | "supersedes"))
                || m.get("id") != Some(&json!(id))
            {
                return Err("Invalid memory tombstone".into());
            }
            if let Some(s) = m.get("supersedes").filter(|v| !v.is_null()) {
                if fmt::id(s.as_str().ok_or("Invalid memory tombstone")?)? == id {
                    return Err("Memory entry cannot supersede itself".into());
                }
            }
            Ok(v)
        })
        .transpose()?;
    Ok((raw, value, old))
}
fn tomb_fields(id: &str, approval: Option<&Fields>) -> Value {
    match approval {
        None => json!({"id":id}),
        Some(a) => json!({"id":id,"supersedes":a.get("supersedes")}),
    }
}
fn confirmed(canonical: Option<&Value>, old: Option<&Fields>) -> Option<Value> {
    canonical
        .filter(|v| v.get("supersedes").is_some())
        .cloned()
        .or_else(|| {
            old.filter(|f| f.contains_key("supersedes"))
                .map(|f| tomb_fields(&f["id"], Some(f)))
        })
}
fn record_approval(path: &Path, id: &str) -> Result<Option<Fields>> {
    if !io::file(path)? {
        return Ok(None);
    }
    let raw = match io::read(path) {
        Ok(v) => v,
        Err(e) if e == "Memory file is too large" || e == "Invalid encoding in memory file" => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    Ok(raw
        .and_then(|s| fmt::parse(&s, id, MEMORY, false).ok())
        .and_then(|f| fmt::inline(&f)))
}
fn find_approval(mut known: Option<Fields>, paths: &[PathBuf], id: &str) -> Result<Option<Fields>> {
    for path in paths {
        if known.is_some() {
            break;
        }
        known = record_approval(path, id)?;
    }
    Ok(known)
}
fn forgotten(root: &Path, id: &str) -> Result<bool> {
    Ok(io::file(&legacy(root, "Forgotten", id))? || io::file(&tombstone(root, id))?)
}
fn write_tombstone(root: &Path, id: &str, raw: Option<&str>, v: &Value) -> Result<()> {
    if !io::write(&tombstone(root, id), &format!("{v}\n"), raw.is_some(), raw)? {
        return Err("Memory tombstone changed while forgetting an entry".into());
    }
    Ok(())
}
pub fn update(
    path: &Path,
    raw: &str,
    fields: &Fields,
    updates: &BTreeMap<String, Option<String>>,
) -> Result<bool> {
    let mut pending = updates
        .iter()
        .filter(|(k, v)| fields.get(*k) != v.as_ref())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();
    if pending.is_empty() {
        return Ok(false);
    }
    let (meta, body) = raw
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n"))
        .ok_or("Invalid memory metadata")?;
    let mut lines = Vec::new();
    for line in meta.lines() {
        let key = line.split_once(": ").map(|v| v.0).unwrap_or(line);
        match pending.remove(key) {
            None => lines.push(line.into()),
            Some(Some(value)) => lines.push(format!("{key}: {}", json!(value))),
            Some(None) => {}
        }
    }
    for (k, v) in pending {
        if let Some(v) = v {
            lines.push(format!("{k}: {}", json!(v)));
        }
    }
    let text = format!("---\n{}\n---\n{body}", lines.join("\n"));
    if text.len() > 32768 {
        return Err("Updated memory note is too large".into());
    }
    io::write(path, &text, true, Some(raw))
}
fn graph(
    root: &Path,
    predecessor: Option<&String>,
    link: Option<String>,
) -> BTreeMap<String, Option<String>> {
    BTreeMap::from([
        ("project_link".into(), link),
        (
            "supersedes_link".into(),
            predecessor.map(|s| {
                format!(
                    "[[Codex Memory/Projects/{}/Records/{s}]]",
                    root.file_name().unwrap().to_string_lossy()
                )
            }),
        ),
    ])
}
impl Store {
    pub fn new(state: Option<PathBuf>, package: &Path) -> Result<Self> {
        let dir = match state {
            Some(p) => p,
            None => std::env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or(io::home()?.join(".codex"))
                .join("fstack/memory"),
        };
        Ok(Self {
            state: io::absolute(&dir)?,
            package: package.into(),
        })
    }
    fn check_home(&self, root: &Path) -> Result<()> {
        if root.file_name().is_none_or(|s| s != "Codex Memory")
            || !views::marker(
                io::read(&root.join("Home.md"))?.as_deref(),
                "fstack_memory_home: 1",
            )
        {
            return Err("Invalid memory home".into());
        }
        Ok(())
    }
    fn read_binding(&self, path: &Path) -> Result<Option<Binding>> {
        let Some(raw) = io::read(path)? else {
            return Ok(None);
        };
        let v: Value = serde_json::from_str(&raw).map_err(io::err)?;
        let root = PathBuf::from(v["root"].as_str().ok_or("Invalid memory binding")?);
        let project = fmt::project(v["project"].as_str().ok_or("Invalid memory binding")?)?.into();
        let mode = v["mode"]
            .as_str()
            .filter(|s| matches!(*s, "auto" | "inbox"))
            .ok_or("Invalid memory mode")?
            .into();
        if !root.is_absolute()
            || !io::equal(&root, &io::absolute(&root)?)
            || root.file_name().is_none_or(|s| s != "Codex Memory")
        {
            return Err("Invalid memory root path".into());
        }
        Ok(Some(Binding {
            root,
            project,
            mode,
        }))
    }
    pub fn resolve(&self, cwd: &Path) -> Result<Option<Binding>> {
        let binding = self.read_binding(&scope::binding_path(&self.state, cwd)?)?;
        if let Some(b) = &binding {
            self.check_home(&b.root)?;
        }
        Ok(binding)
    }
    pub fn disable(&self, cwd: &Path) -> Result<bool> {
        io::delete(&scope::binding_path(&self.state, cwd)?)
    }
    pub fn root(&self, b: &Binding) -> Result<PathBuf> {
        self.check_home(&b.root)?;
        Ok(b.root.join("Projects").join(fmt::project(&b.project)?))
    }
    pub fn setup(
        &self,
        vault: &Path,
        repo: &Path,
        project: &str,
        mode: Option<&str>,
    ) -> Result<Binding> {
        fmt::project(project)?;
        let settings = scope::binding_path(&self.state, repo)?;
        let prior = self.read_binding(&settings)?;
        let mode = mode
            .or(prior.as_ref().map(|b| b.mode.as_str()))
            .unwrap_or("auto");
        if !matches!(mode, "auto" | "inbox") {
            return Err("Invalid memory mode".into());
        }
        let vault = io::absolute(vault)?;
        if !io::directory(&vault.join(".obsidian"), false, false)? {
            return Err("Selected directory is not an Obsidian vault".into());
        }
        let root = vault.join("Codex Memory");
        io::directory(&root, true, false)?;
        let home = io::read(&root.join("Home.md"))?;
        if home.is_some() || fs::read_dir(&root).map_err(io::err)?.next().is_some() {
            self.check_home(&root)?;
        }
        for relative in views::VAULT_FILES {
            if *relative != "Home.md" {
                self.check_home(&root)?;
            }
            let path = root.join(relative);
            io::directory(path.parent().unwrap(), true, false)?;
            let text = views::asset(&self.package, relative)?;
            if *relative == "Home.md" && !views::marker(Some(&text), "fstack_memory_home: 1") {
                return Err("Invalid memory home marker".into());
            }
            io::write(&path, &text, false, None)?;
            self.check_home(&root)?;
        }
        let b = Binding {
            root,
            project: project.into(),
            mode: mode.into(),
        };
        let p = self.root(&b)?;
        io::directory(&p, true, false)?;
        let _lock = io::lock(&p, &vault)?;
        views::ensure_hub(&b.root, &self.package)?;
        let _ = views::ensure_library(&b.root, &self.package);
        if views::current_page(&p)?.is_none() {
            let page = views::project_page(&p);
            if io::file(&page)? || io::file(&p.join("Project.md"))? {
                return Err("Memory project page is occupied".into());
            }
            if io::directory(&p.join("Records"), false, false)? {
                return Err("Memory project page is unavailable".into());
            }
            io::write(
                &page,
                &views::asset(&self.package, "Project.md")?.replace("@PROJECT@", project),
                false,
                None,
            )?;
            views::current_link(&p)?;
        }
        io::directory(&p.join("Records"), true, false)?;
        io::directory(settings.parent().unwrap(), true, true)?;
        io::write(
            &settings,
            &json!({"root":b.root,"project":b.project,"mode":b.mode}).to_string(),
            true,
            None,
        )?;
        Ok(b)
    }
    pub fn statuses(&self, root: &Path) -> Result<(BTreeMap<String, Fields>, BTreeSet<String>)> {
        let mut approvals = BTreeMap::new();
        for p in files(&root.join("Approved"), false)? {
            let id = stem(&p);
            if let Some(a) = note(&p, &id, APPROVED, true)? {
                approvals.insert(id, a);
            }
        }
        let mut inline = BTreeMap::new();
        let mut unreadable = BTreeMap::new();
        for p in files(&root.join("Records"), false)? {
            let id = stem(&p);
            if approvals.contains_key(&id) || !io::file(&p)? {
                continue;
            }
            let raw = match io::read(&p) {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    unreadable.insert(id, e);
                    continue;
                }
            };
            if let Some(a) = fmt::parse(&raw, &id, MEMORY, false)
                .ok()
                .and_then(|f| fmt::inline(&f))
            {
                inline.insert(id, a);
            }
        }
        let mut tombstones = BTreeMap::new();
        for id in tombstone_ids(root)? {
            let (_, canonical, old) = read_tombstones(root, &id)?;
            if canonical.is_none() && old.is_none() {
                continue;
            }
            let mut conf = confirmed(canonical.as_ref(), old.as_ref());
            if conf.is_none() && !approvals.contains_key(&id) {
                if let Some(a) = record_approval(&captured(root, &id), &id)? {
                    inline.insert(id.clone(), a);
                }
                let (_, c, l) = read_tombstones(root, &id)?;
                conf = confirmed(c.as_ref(), l.as_ref());
            }
            tombstones.insert(id.clone(), conf.unwrap_or(json!({"id":id})));
        }
        for (id, e) in unreadable {
            if !tombstones.contains_key(&id) {
                return Err(e);
            }
        }
        approvals.extend(inline);
        let mut hidden = tombstones.keys().cloned().collect::<BTreeSet<_>>();
        let mut predecessors = approvals
            .iter()
            .filter_map(|(id, a)| a.get("supersedes").map(|s| (id.clone(), s.clone())))
            .collect::<BTreeMap<_, _>>();
        for (id, v) in &tombstones {
            if v.get("supersedes").is_some() {
                predecessors.remove(id);
                if let Some(s) = v["supersedes"].as_str() {
                    predecessors.insert(id.clone(), s.into());
                }
            }
        }
        hidden.extend(predecessors.into_values());
        Ok((approvals, hidden))
    }
    fn public_record(
        &self,
        root: &Path,
        id: &str,
        approval: Option<&Fields>,
    ) -> Result<Option<Value>> {
        if forgotten(root, id)? {
            return Ok(None);
        }
        let f = note(&record(root, id), id, MEMORY, true)?;
        if forgotten(root, id)? {
            return Ok(None);
        }
        Ok(f.map(|f| fmt::public(f, approval)))
    }
    pub fn get(&self, b: &Binding, id: &str) -> Result<Value> {
        fmt::id(id)?;
        let root = self.root(b)?;
        let (a, h) = self.statuses(&root)?;
        if !h.contains(id) {
            if let Some(a) = a.get(id) {
                if let Some(v) = self.public_record(&root, id, Some(a))? {
                    return Ok(v);
                }
            }
        }
        Err("Memory entry is unavailable".into())
    }
    pub fn inbox(&self, b: &Binding) -> Result<Value> {
        let root = self.root(b)?;
        let (a, h) = self.statuses(&root)?;
        let mut entries = Vec::new();
        for p in files(&root.join("Records"), false)? {
            let id = stem(&p);
            if !a.contains_key(&id) && !h.contains(&id) {
                if let Some(v) = self.public_record(&root, &id, None)? {
                    entries.push(v);
                }
            }
        }
        entries.sort_by(|a, b| {
            (a["created"].as_str().unwrap_or(""), a["id"].as_str())
                .cmp(&(b["created"].as_str().unwrap_or(""), b["id"].as_str()))
        });
        Ok(json!(entries))
    }
    fn check_supersedes(
        &self,
        root: &Path,
        f: &Fields,
        a: &BTreeMap<String, Fields>,
        h: &BTreeSet<String>,
    ) -> Result<()> {
        if let Some(p) = f.get("supersedes") {
            if !a.contains_key(p) || h.contains(p) || !io::file(&record(root, p))? {
                return Err("Superseded memory entry is unavailable".into());
            }
        }
        Ok(())
    }
    pub fn save(
        &self,
        b: &Binding,
        value: &Value,
        approved: bool,
        session: Option<&str>,
        turn: Option<&str>,
    ) -> Result<Value> {
        let fields = fmt::candidate(value)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let _ = views::ensure_library(&b.root, &self.package);
        let link = views::current_link(&root)?;
        let id = fmt::stable_id(&fields);
        let mut provenance = Fields::new();
        for (k, v) in [("session_id", session), ("turn_id", turn)] {
            if let Some(v) = v {
                provenance.insert(k.into(), fmt::safe(&json!(v), 128, false)?);
            }
        }
        let (a, h) = self.statuses(&root)?;
        if h.contains(&id) {
            return Err("Memory entry is unavailable".into());
        }
        let approved = approved && b.mode == "auto";
        if !a.contains_key(&id) && !io::file(&record(&root, &id))? {
            self.check_supersedes(&root, &fields, &a, &h)?;
            let mut metadata = fields.clone();
            metadata.remove("body");
            metadata.insert("id".into(), id.clone());
            metadata.insert(
                "created".into(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            );
            metadata.extend(provenance);
            for (k, v) in graph(&root, fields.get("supersedes"), Some(link)) {
                if let Some(v) = v {
                    metadata.insert(k, v);
                }
            }
            if approved {
                metadata.insert("fstack_status".into(), "approved".into());
            }
            if io::write(
                &record(&root, &id),
                &fmt::markdown(MEMORY, &metadata, &fields["body"]),
                false,
                None,
            )? {
                if forgotten(&root, &id)? {
                    self.forget_locked(&root, &id)?;
                    return Err("Memory entry is unavailable".into());
                }
                let approval = fmt::inline(&metadata);
                metadata.insert("body".into(), fields["body"].clone());
                return Ok(fmt::public(metadata, approval.as_ref()));
            }
        }
        if approved {
            return self.approve_locked(&root, &id);
        }
        let (a, h) = self.statuses(&root)?;
        if h.contains(&id) {
            return Err("Memory entry is unavailable".into());
        }
        self.public_record(&root, &id, a.get(&id))?
            .ok_or("Memory entry is unavailable".into())
    }
    pub fn approve(&self, b: &Binding, id: &str) -> Result<Value> {
        fmt::id(id)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let _ = views::ensure_library(&b.root, &self.package);
        self.approve_locked(&root, id)
    }
    fn approve_locked(&self, root: &Path, id: &str) -> Result<Value> {
        let link = views::current_link(root)?;
        let (a, h) = self.statuses(root)?;
        if h.contains(id) {
            return Err("Memory entry is unavailable".into());
        }
        let p = record(root, id);
        let raw = io::read(&p)?.ok_or("Inbox entry is unavailable")?;
        if forgotten(root, id)? {
            return Err("Inbox entry is unavailable".into());
        }
        let fields = fmt::parse(&raw, id, MEMORY, true)?;
        let approval = match a.get(id) {
            Some(a) => a.clone(),
            None => {
                self.check_supersedes(root, &fields, &a, &h)?;
                fmt::approval(&fields)
            }
        };
        let mut updates = graph(root, approval.get("supersedes"), Some(link));
        updates.insert("fstack_status".into(), Some("approved".into()));
        updates.insert("supersedes".into(), approval.get("supersedes").cloned());
        update(&p, &raw, &fields, &updates)?;
        if forgotten(root, id)? {
            return Err("Memory entry is unavailable".into());
        }
        Ok(fmt::public(fields, Some(&approval)))
    }
    pub fn forget(&self, b: &Binding, id: &str) -> Result<Value> {
        fmt::id(id)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        self.forget_locked(&root, id)
    }
    fn finish_forget(&self, root: &Path, id: &str, approval: Option<&Fields>) -> Result<()> {
        let (raw, c, l) = read_tombstones(root, id)?;
        let c = c.ok_or("Memory tombstone disappeared during removal")?;
        if c.get("supersedes").is_none() {
            let conf = match confirmed(Some(&c), l.as_ref()) {
                Some(v) => v,
                None => {
                    let a = find_approval(approval.cloned(), &[captured(root, id)], id)?;
                    tomb_fields(id, a.as_ref())
                }
            };
            if conf != c {
                write_tombstone(root, id, raw.as_deref(), &conf)?;
            }
        }
        io::delete(&captured(root, id))?;
        Ok(())
    }
    fn forget_locked(&self, root: &Path, id: &str) -> Result<Value> {
        let (raw, c, l) = read_tombstones(root, id)?;
        let marker = legacy(root, "Approved", id);
        let has_marker = io::file(&marker)?;
        let has_record = io::file(&record(root, id))?;
        let approval = if has_marker {
            note(&marker, id, APPROVED, true).ok().flatten()
        } else {
            None
        };
        if raw.is_none() {
            if l.is_none() && !has_record && !has_marker {
                return Err("Memory entry is unavailable".into());
            }
            if l.is_none() {
                let mut ids = tombstone_ids(root)?;
                ids.insert(id.into());
                if ids.len() > 4096 {
                    return Err("Memory tombstone entry limit exceeded".into());
                }
            }
            let observed = match confirmed(c.as_ref(), l.as_ref()) {
                Some(v) => v,
                None => {
                    let a = find_approval(
                        approval.clone(),
                        &[captured(root, id), record(root, id)],
                        id,
                    )?;
                    tomb_fields(id, a.as_ref())
                }
            };
            write_tombstone(root, id, None, &observed)?;
        }
        self.finish_forget(root, id, approval.as_ref())?;
        if io::file(&record(root, id))? {
            if io::file(&captured(root, id))? {
                return Err("Previous memory removal is incomplete".into());
            }
            match fs::rename(record(root, id), captured(root, id)) {
                Ok(()) => self.finish_forget(root, id, approval.as_ref())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io::err(e)),
            }
        }
        Ok(json!({"id":id,"status":"forgotten"}))
    }
    pub fn search(&self, b: &Binding, query: &str, limit: usize) -> Result<Value> {
        use unicode_casefold::UnicodeCaseFold;
        if query.chars().count() > 500 || !(1..=50).contains(&limit) {
            return Err("Invalid memory search query or limit".into());
        }
        let stop="a an and are as at be been but by can could did do does for from had has have how i if in into is it its me my of on or our should that the their this to us was we were what when where which who why will with would you your а без был была были было в во вот для до его ее её если есть же за и из или к как какая какие какой какое ко когда кто ли мне мой мы на над нам наш не ни но о об от по под при про с со так там то тот у уже что чтобы это этого этой этом этот я".split_whitespace().collect::<BTreeSet<_>>();
        let q = query.case_fold().collect::<String>();
        let regex = regex::Regex::new(r"[\p{L}\p{N}]+").unwrap();
        let mut terms = Vec::new();
        for m in regex.find_iter(&q) {
            if !stop.contains(m.as_str()) && !terms.contains(&m.as_str().to_owned()) {
                terms.push(m.as_str().to_owned());
                if terms.len() == 20 {
                    break;
                }
            }
        }
        if terms.is_empty() {
            return Ok(json!([]));
        }
        let root = self.root(b)?;
        let (a, h) = self.statuses(&root)?;
        let conn = rusqlite::Connection::open_in_memory().map_err(io::err)?;
        conn.execute_batch("CREATE VIRTUAL TABLE entries USING fts5(id UNINDEXED,title,body)")
            .map_err(io::err)?;
        let mut entries = BTreeMap::new();
        for (id, a) in a {
            if h.contains(&id) {
                continue;
            }
            if let Some(v) = self.public_record(&root, &id, Some(&a))? {
                conn.execute(
                    "INSERT INTO entries VALUES (?,?,?)",
                    rusqlite::params![
                        id,
                        v["title"].as_str().unwrap().case_fold().collect::<String>(),
                        v["body"].as_str().unwrap().case_fold().collect::<String>()
                    ],
                )
                .map_err(io::err)?;
                entries.insert(id, v);
            }
        }
        let expression = terms
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt=conn.prepare("SELECT id FROM entries WHERE entries MATCH ? ORDER BY bm25(entries,0,2,1),id LIMIT ?").map_err(io::err)?;
        let ids = stmt
            .query_map(rusqlite::params![expression, limit as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(io::err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io::err)?;
        Ok(json!(ids
            .iter()
            .map(|id| entries[id].clone())
            .collect::<Vec<_>>()))
    }
    pub fn migrate(&self, b: &Binding, prune: bool) -> Result<Value> {
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let _ = views::ensure_library(&b.root, &self.package);
        if prune {
            return self.prune(&root);
        }
        views::ensure_hub(&b.root, &self.package)?;
        let (mut updated, mut skipped) = views::migrate_project(&root)?;
        let page = views::current_page(&root)?;
        let named = page.as_ref() == Some(&views::project_page(&root));
        let link = page.as_ref().map(|p| views::project_link(p));
        let mut ids = tombstone_ids(&root)?;
        for folder in ["Records", "Approved"] {
            ids.extend(files(&root.join(folder), false)?.iter().map(|p| stem(p)));
        }
        for id in ids {
            let p = record(&root, &id);
            let prepared = (|| -> Result<_> {
                let mut a = note(&legacy(&root, "Approved", &id), &id, APPROVED, true)?;
                let (raw, c, l) = read_tombstones(&root, &id)?;
                if c.is_some() || l.is_some() {
                    let conf = confirmed(c.as_ref(), l.as_ref());
                    if a.is_none() {
                        a = find_approval(None, &[captured(&root, &id), p.clone()], &id)?;
                    }
                    let desired = conf.clone().unwrap_or_else(|| tomb_fields(&id, a.as_ref()));
                    let conflict = conf.as_ref().is_some_and(|v| {
                        l.as_ref()
                            .filter(|l| l.contains_key("supersedes"))
                            .is_some_and(|l| *v != json!(l))
                            || a.as_ref().is_some_and(|a| *v != tomb_fields(&id, Some(a)))
                    });
                    Ok((raw, c, Some(desired), conflict, None))
                } else {
                    let text = io::read(&p)?.ok_or("Missing record")?;
                    let fields = fmt::parse(&text, &id, MEMORY, true)?;
                    let pred = a.as_ref().unwrap_or(&fields).get("supersedes");
                    let mut updates = graph(&root, pred, link.clone());
                    if link.is_none() || (!named && fields.contains_key("project_link")) {
                        updates.remove("project_link");
                    }
                    if a.is_some() {
                        updates.insert("fstack_status".into(), Some("approved".into()));
                        updates.insert("supersedes".into(), pred.cloned());
                    }
                    Ok((None, None, None, false, Some((text, fields, updates))))
                }
            })();
            let (raw, c, desired, conflict, normal) = match prepared {
                Ok(v) => v,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if let Some(desired) = desired {
                let changed = c.as_ref() != Some(&desired);
                if changed {
                    write_tombstone(&root, &id, raw.as_deref(), &desired)?;
                }
                let mut record_changed = false;
                if named {
                    if let Ok(Some(raw)) = io::read(&p) {
                        if let Ok(fields) = fmt::parse(&raw, &id, MEMORY, true) {
                            record_changed = update(
                                &p,
                                &raw,
                                &fields,
                                &BTreeMap::from([("project_link".into(), link.clone())]),
                            )?;
                        }
                    }
                }
                updated += usize::from(changed || record_changed);
                skipped += usize::from(conflict && !changed && !record_changed);
            } else if let Some((raw, fields, updates)) = normal {
                updated += usize::from(update(&p, &raw, &fields, &updates)?);
            }
        }
        Ok(json!({"updated":updated,"skipped":skipped}))
    }
    fn prune(&self, root: &Path) -> Result<Value> {
        let (mut pruned, mut skipped) = (0, 0);
        for (folder, marker) in [("Approved", APPROVED), ("Forgotten", FORGOTTEN)] {
            let dir = root.join(folder);
            let mut paths = files(&dir, false)?;
            paths.sort();
            for p in paths {
                let checked = (|| -> Result<bool> {
                    let Some(raw) = io::read(&p)? else {
                        return Ok(false);
                    };
                    let id = stem(&p);
                    let fields = fmt::parse(&raw, &id, marker, true)?;
                    let (_, c, l) = read_tombstones(root, &id)?;
                    let equivalent = if marker == FORGOTTEN {
                        c.as_ref().is_some_and(|v| {
                            !fields.contains_key("supersedes") || *v == json!(fields)
                        })
                    } else if c.is_some() || l.is_some() {
                        c == Some(tomb_fields(&id, Some(&fields)))
                    } else {
                        note(&record(root, &id), &id, MEMORY, true)?.and_then(|f| fmt::inline(&f))
                            == Some(fields)
                    };
                    Ok(equivalent && io::read(&p)?.as_deref() == Some(&raw))
                })();
                match checked {
                    Ok(true) => pruned += usize::from(io::delete(&p)?),
                    _ => skipped += 1,
                }
            }
            if io::directory(&dir, false, false)? {
                match fs::remove_dir(&dir) {
                    Ok(()) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(e) => return Err(io::err(e)),
                }
            }
        }
        let (p, s) = views::prune_project(root)?;
        Ok(json!({"pruned":pruned+p,"skipped":skipped+s}))
    }
}
