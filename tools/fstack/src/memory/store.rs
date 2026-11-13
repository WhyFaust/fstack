use super::{
    auto,
    format::{self as fmt, Fields, APPROVED, FORGOTTEN, MEMORY},
    io, scope, views, Result,
};
use rust_stemmers::{Algorithm, Stemmer};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
fn russian_stem(stemmer: &Stemmer, word: &str) -> Option<String> {
    if !word.chars().all(|ch| matches!(ch, 'а'..='я' | 'ё')) {
        return None;
    }
    let normalized = word.replace('ё', "е");
    let stem = stemmer.stem(&normalized);
    (stem.chars().count() >= 3).then(|| stem.into_owned())
}

fn fts_alternatives<'a>(words: impl IntoIterator<Item = &'a str>) -> String {
    words
        .into_iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub root: PathBuf,
    pub project: String,
    pub mode: String,
    pub generation: Option<String>,
    pub(super) shared: bool,
}
pub enum WriteOutcome {
    Written(Value),
    Forgotten,
}
impl WriteOutcome {
    fn into_record(self) -> Result<Value> {
        match self {
            Self::Written(record) => Ok(record),
            Self::Forgotten => Err("Memory entry is unavailable".into()),
        }
    }
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
const ENTRY_LIMIT: usize = 4096;

pub fn files(dir: &Path, tombstones: bool) -> Result<Vec<PathBuf>> {
    if !io::directory(dir, false, false)? {
        return Ok(Vec::new());
    }
    let records = dir.file_name().is_some_and(|s| s == "Records");
    let suffix = if tombstones { "-forgotten.json" } else { ".md" };
    let mut result = Vec::new();
    for entry in fs::read_dir(dir).map_err(io::err)? {
        let path = entry.map_err(io::err)?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name
            .strip_suffix(suffix)
            .is_some_and(|id| fmt::id(id).is_ok())
        {
            result.push(path);
            if (tombstones || !records) && result.len() > ENTRY_LIMIT {
                return Err("Memory directory entry limit exceeded".into());
            }
        }
    }
    Ok(result)
}
pub(super) fn validate_history(id: &str, limit: usize) -> Result<()> {
    fmt::id(id)?;
    if !(1..=10).contains(&limit) {
        return Err("Invalid memory history limit".into());
    }
    Ok(())
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
    if ids.len() > ENTRY_LIMIT {
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
fn verified_forgotten(root: &Path, id: &str) -> Result<bool> {
    let (_, canonical, legacy) = read_tombstones(root, id)?;
    Ok(canonical.is_some() || legacy.is_some())
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
                if super::shared::is_root(root) {
                    return format!("[[Codex Memory/Shared/Records/{s}]]");
                }
                format!(
                    "[[Codex Memory/Projects/{}/Records/{s}]]",
                    root.file_name().unwrap().to_string_lossy()
                )
            }),
        ),
    ])
}
fn history_diff(previous: &Value, current: &Value) -> Value {
    let mut changes = serde_json::Map::new();
    for field in ["kind", "title", "source"] {
        if previous[field] != current[field] {
            changes.insert(
                field.into(),
                json!({"from":previous[field],"to":current[field]}),
            );
        }
    }
    if previous["body"] != current["body"] {
        let before = previous["body"]
            .as_str()
            .unwrap_or("")
            .split('\n')
            .collect::<Vec<_>>();
        let after = current["body"]
            .as_str()
            .unwrap_or("")
            .split('\n')
            .collect::<Vec<_>>();
        let shared_start = before
            .iter()
            .zip(&after)
            .take_while(|(a, b)| a == b)
            .count();
        let shared_end = before[shared_start..]
            .iter()
            .rev()
            .zip(after[shared_start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        changes.insert(
            "body".into(),
            json!({
                "start_line":shared_start+1,
                "removed":before[shared_start..before.len()-shared_end],
                "added":after[shared_start..after.len()-shared_end],
            }),
        );
    }
    json!({"from":previous["id"],"to":current["id"],"fields":changes})
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
    pub(super) fn check_home(&self, root: &Path) -> Result<()> {
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
    pub(super) fn read_binding(&self, path: &Path) -> Result<Option<Binding>> {
        let Some(raw) = io::read(path)? else {
            return Ok(None);
        };
        let v: Value = serde_json::from_str(&raw).map_err(io::err)?;
        if v == json!({"disabled":true}) {
            return Ok(None);
        }
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
            shared: false,
            root,
            project,
            mode,
            generation: v
                .get("generation")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|s| fmt::matches(r"\A[0-9a-f]{32}\z", s))
                        .map(str::to_owned)
                        .ok_or("Invalid memory binding generation")
                })
                .transpose()?,
        }))
    }
    pub fn bound(&self, cwd: &Path) -> Result<Option<Binding>> {
        self.read_binding(&scope::binding_path(&self.state, cwd)?)
    }
    pub fn resolve(&self, cwd: &Path) -> Result<Option<Binding>> {
        let binding = self.bound(cwd)?;
        if let Some(b) = &binding {
            self.check_home(&b.root)?;
        }
        Ok(binding)
    }
    pub fn lock_binding(&self, path: &Path) -> Result<fs::File> {
        let key = io::hash(format!("memory-binding:{}", io::normalized(path)));
        io::lock_key(&key[..2], None)
    }
    pub fn disable(&self, cwd: &Path) -> Result<bool> {
        let path = scope::binding_path(&self.state, cwd)?;
        let _binding_lock = self.lock_binding(&path)?;
        io::state(&self.state, cwd, true)?;
        io::write(&path, r#"{"disabled":true}"#, true, None)?;
        Ok(true)
    }
    pub fn root(&self, b: &Binding) -> Result<PathBuf> {
        self.check_home(&b.root)?;
        if b.shared {
            return Ok(b.root.join("Shared"));
        }
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
        let _binding_lock = self.lock_binding(&settings)?;
        let prior = self.read_binding(&settings)?;
        self.setup_locked(vault, project, mode, &settings, prior, None)
    }
    pub(super) fn setup_locked(
        &self,
        vault: &Path,
        project: &str,
        mode: Option<&str>,
        settings: &Path,
        prior: Option<Binding>,
        owner: Option<&str>,
    ) -> Result<Binding> {
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
        let _vault_lock = io::lock(&vault, &vault)?;
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
        let project = match owner {
            Some(hash) => auto::claim_project(&root, project, hash)?,
            None => project.to_owned(),
        };
        let generation = match prior.as_ref() {
            Some(previous)
                if io::equal(&previous.root, &root)
                    && previous.project == project
                    && previous.mode == mode =>
            {
                previous.generation.clone()
            }
            _ => Some(io::nonce()?),
        };
        let b = Binding {
            shared: false,
            root,
            project: project.clone(),
            mode: mode.into(),
            generation,
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
                &views::asset(&self.package, "Project.md")?.replace("@PROJECT@", &project),
                false,
                None,
            )?;
            views::current_link(&p)?;
        }
        io::directory(&p.join("Records"), true, false)?;
        io::directory(settings.parent().unwrap(), true, true)?;
        let mut settings_json = json!({"root":b.root,"project":b.project,"mode":b.mode});
        if let Some(generation) = &b.generation {
            settings_json["generation"] = json!(generation);
        }
        io::write(settings, &settings_json.to_string(), true, None)?;
        Ok(b)
    }
    pub fn statuses(&self, root: &Path) -> Result<(BTreeMap<String, Fields>, BTreeSet<String>)> {
        self.record_statuses(root, files(&root.join("Records"), false)?)
    }

    fn record_statuses(
        &self,
        root: &Path,
        records: Vec<PathBuf>,
    ) -> Result<(BTreeMap<String, Fields>, BTreeSet<String>)> {
        let mut approvals = BTreeMap::new();
        for p in files(&root.join("Approved"), false)? {
            let id = stem(&p);
            if let Some(a) = note(&p, &id, APPROVED, true)? {
                approvals.insert(id, a);
            }
        }
        let mut inline = BTreeMap::new();
        let mut unreadable = BTreeMap::new();
        for p in records {
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
    pub(super) fn public_record(
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
    pub fn history(&self, b: &Binding, id: &str, limit: usize) -> Result<Value> {
        validate_history(id, limit)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let (approvals, _) = self.statuses(&root)?;
        let tombstones = tombstone_ids(&root)?;
        if tombstones.contains(id) || !approvals.contains_key(id) {
            return Err("Memory entry is unavailable".into());
        }
        let mut successors = BTreeMap::<&str, Vec<&str>>::new();
        for (child, fields) in &approvals {
            if !tombstones.contains(child) {
                if let Some(parent) = fields.get("supersedes") {
                    if !tombstones.contains(parent) {
                        successors.entry(parent).or_default().push(child);
                    }
                }
            }
        }

        let mut entries = Vec::new();
        let mut positions = BTreeMap::new();
        let mut seen = BTreeSet::from([id]);
        let mut queue = std::collections::VecDeque::from([id]);
        let mut truncated = false;
        while let Some(current) = queue.pop_front() {
            let Some(approval) = approvals.get(current) else {
                continue;
            };
            let Some(mut value) = self.public_record(&root, current, Some(approval))? else {
                continue;
            };
            if entries.len() == limit {
                truncated = true;
                break;
            }
            let predecessor = approval
                .get("supersedes")
                .map(String::as_str)
                .filter(|parent| !tombstones.contains(*parent));
            if predecessor.is_none() {
                value.as_object_mut().unwrap().remove("supersedes");
            }
            positions.insert(current, entries.len());
            entries.push(value);
            for next in predecessor
                .into_iter()
                .chain(successors.get(current).into_iter().flatten().copied())
            {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        if entries.is_empty() {
            return Err("Memory entry is unavailable".into());
        }
        let changes = entries
            .iter()
            .filter_map(|entry| {
                let predecessor = entry["supersedes"].as_str()?;
                let previous = &entries[*positions.get(predecessor)?];
                Some(history_diff(previous, entry))
            })
            .collect::<Vec<_>>();
        Ok(json!({"entries":entries,"changes":changes,"truncated":truncated}))
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
        self.save_outcome(b, value, approved, session, turn)?
            .into_record()
    }
    pub fn save_outcome(
        &self,
        b: &Binding,
        value: &Value,
        approved: bool,
        session: Option<&str>,
        turn: Option<&str>,
    ) -> Result<WriteOutcome> {
        let fields = fmt::candidate(value)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let id = fmt::stable_id(&fields);
        if verified_forgotten(&root, &id)? {
            return Ok(WriteOutcome::Forgotten);
        }
        let _ = views::ensure_library(&b.root, &self.package);
        let link = views::current_link(&root)?;
        let mut provenance = Fields::new();
        for (k, v) in [("session_id", session), ("turn_id", turn)] {
            if let Some(v) = v {
                provenance.insert(k.into(), fmt::safe(&json!(v), 128, false)?);
            }
        }
        let path = record(&root, &id);
        let records = files(&root.join("Records"), false)?;
        let record_count = records.len();
        let (a, h) = self.record_statuses(&root, records)?;
        if h.contains(&id) {
            return Err("Memory entry is unavailable".into());
        }
        let approved = approved && b.mode == "auto";
        if !a.contains_key(&id) && !io::file(&path)? {
            if record_count >= ENTRY_LIMIT {
                return Err(
                    "Memory record limit reached; forget an entry before saving a new one".into(),
                );
            }
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
                &path,
                &fmt::markdown(MEMORY, &metadata, &fields["body"]),
                false,
                None,
            )? {
                if verified_forgotten(&root, &id)? {
                    self.forget_locked(&root, &id)?;
                    return Ok(WriteOutcome::Forgotten);
                }
                let approval = fmt::inline(&metadata);
                metadata.insert("body".into(), fields["body"].clone());
                return Ok(WriteOutcome::Written(fmt::public(
                    metadata,
                    approval.as_ref(),
                )));
            }
        }
        if approved {
            return self.approve_locked(&root, &id);
        }
        let (a, h) = self.statuses(&root)?;
        if h.contains(&id) {
            return Err("Memory entry is unavailable".into());
        }
        let entry = self.public_record(&root, &id, a.get(&id))?;
        if verified_forgotten(&root, &id)? {
            return Ok(WriteOutcome::Forgotten);
        }
        entry
            .map(WriteOutcome::Written)
            .ok_or("Memory entry is unavailable".into())
    }
    pub fn approve(&self, b: &Binding, id: &str) -> Result<Value> {
        self.approve_outcome(b, id)?.into_record()
    }
    pub fn approve_outcome(&self, b: &Binding, id: &str) -> Result<WriteOutcome> {
        fmt::id(id)?;
        let root = self.root(b)?;
        let _lock = io::lock(&root, b.root.parent().unwrap())?;
        let _ = views::ensure_library(&b.root, &self.package);
        self.approve_locked(&root, id)
    }
    fn approve_locked(&self, root: &Path, id: &str) -> Result<WriteOutcome> {
        if verified_forgotten(root, id)? {
            return Ok(WriteOutcome::Forgotten);
        }
        let link = views::current_link(root)?;
        let (a, h) = self.statuses(root)?;
        if h.contains(id) {
            return Err("Memory entry is unavailable".into());
        }
        let p = record(root, id);
        let raw = io::read(&p)?;
        if verified_forgotten(root, id)? {
            return Ok(WriteOutcome::Forgotten);
        }
        let raw = raw.ok_or("Inbox entry is unavailable")?;
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
        if verified_forgotten(root, id)? {
            return Ok(WriteOutcome::Forgotten);
        }
        Ok(WriteOutcome::Written(fmt::public(fields, Some(&approval))))
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
                if ids.len() > ENTRY_LIMIT {
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
        io::sync_parent(&record(root, id))?;
        Ok(json!({"id":id,"status":"forgotten"}))
    }
    pub fn search(&self, b: &Binding, query: &str, limit: usize) -> Result<Value> {
        self.ranked_records(b, query, limit, false)
    }

    pub(super) fn recall(&self, b: &Binding, query: &str, limit: usize) -> Result<Value> {
        self.ranked_records(b, query, limit, true)
    }

    fn ranked_records(
        &self,
        b: &Binding,
        query: &str,
        limit: usize,
        fill_with_rules: bool,
    ) -> Result<Value> {
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
        if terms.is_empty() && !fill_with_rules {
            return Ok(json!([]));
        }
        let stemmer = Stemmer::create(Algorithm::Russian);
        let stems: BTreeSet<String> = terms
            .iter()
            .filter_map(|word| russian_stem(&stemmer, word))
            .collect();
        let mut expression = format!(
            "{{title body}} : ({})",
            fts_alternatives(terms.iter().map(String::as_str))
        );
        if !stems.is_empty() {
            expression.push_str(&format!(
                " OR forms : ({})",
                fts_alternatives(stems.iter().map(String::as_str))
            ));
        }
        let root = self.root(b)?;
        let (a, h) = self.statuses(&root)?;
        let conn = rusqlite::Connection::open_in_memory().map_err(io::err)?;
        conn.execute_batch("CREATE VIRTUAL TABLE entries USING fts5(id UNINDEXED,title,body,forms,created UNINDEXED)")
            .map_err(io::err)?;
        let mut entries = BTreeMap::new();
        for (id, a) in a {
            if h.contains(&id) {
                continue;
            }
            if let Some(v) = self.public_record(&root, &id, Some(&a))? {
                let title = v["title"].as_str().unwrap().case_fold().collect::<String>();
                let body = v["body"].as_str().unwrap().case_fold().collect::<String>();
                let forms = if stems.is_empty() {
                    String::new()
                } else {
                    regex
                        .find_iter(&title)
                        .chain(regex.find_iter(&body))
                        .filter_map(|m| russian_stem(&stemmer, m.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                conn.execute(
                    "INSERT INTO entries VALUES (?,?,?,?,?)",
                    rusqlite::params![id, title, body, forms, v["created"].as_str()],
                )
                .map_err(io::err)?;
                entries.insert(id, v);
            }
        }
        let mut ids = if terms.is_empty() {
            Vec::new()
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM entries WHERE entries MATCH ?
             ORDER BY bm25(entries,0,2,1,0.35,0), julianday(created) DESC, id LIMIT ?",
                )
                .map_err(io::err)?;
            let matched = stmt
                .query_map(rusqlite::params![expression, limit as i64], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(io::err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(io::err)?;
            matched
        };
        if fill_with_rules && ids.len() < limit {
            let mut recent = entries
                .iter()
                .filter(|(id, record)| {
                    !ids.contains(id)
                        && matches!(record["kind"].as_str(), Some("preference" | "correction"))
                })
                .map(|(id, record)| (std::cmp::Reverse(record["created"].as_str()), id))
                .collect::<Vec<_>>();
            recent.sort_unstable();
            ids.extend(
                recent
                    .into_iter()
                    .take(limit - ids.len())
                    .map(|(_, id)| id.clone()),
            );
        }
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
