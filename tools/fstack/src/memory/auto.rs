use super::{
    io, read_json, scope,
    store::{Binding, Store},
    Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Vault {
    path: PathBuf,
    source: &'static str,
}

fn registry_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let dir = std::env::var_os("APPDATA").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let dir = io::home().ok()?.join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| io::home().ok().map(|home| home.join(".config")))?;
    dir.is_absolute()
        .then(|| dir.join("obsidian/obsidian.json"))
}

fn usable(path: &Path) -> Result<bool> {
    Ok(path.is_absolute()
        && io::equal(path, &io::absolute(path)?)
        && io::directory(&path.join(".obsidian"), false, false)?)
}

fn select(paths: BTreeMap<String, PathBuf>, source: &'static str) -> Result<Option<Vault>> {
    if paths.len() > 1 {
        return Err("Multiple Obsidian vaults available. Select the work vault with memory vault --set <absolute path>".into());
    }
    Ok(paths
        .into_values()
        .next()
        .map(|path| Vault { path, source }))
}

impl Store {
    fn discover(&self, registry: Option<&Path>) -> Result<Option<Vault>> {
        if let Some(text) = io::read_limit(&self.state.join("vault.json"), 32768)? {
            let v = read_json(text.as_bytes(), 32768)?;
            let path = PathBuf::from(v["vault"].as_str().ok_or("Invalid pinned Obsidian vault")?);
            if !usable(&path)? {
                return Err("Pinned Obsidian vault is unavailable".into());
            }
            return Ok(Some(Vault {
                path,
                source: "pinned",
            }));
        }

        let mut paths = BTreeMap::new();
        if io::directory(&self.state, false, true)? {
            for entry in fs::read_dir(&self.state).map_err(io::err)? {
                let path = entry.map_err(io::err)?.path();
                if !path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
                    s.strip_suffix(".json")
                        .is_some_and(|id| super::format::id(id).is_ok())
                }) {
                    continue;
                }
                if let Some(binding) = self.read_binding(&path)? {
                    let vault = binding.root.parent().ok_or("Invalid memory root path")?;
                    paths.insert(io::normalized(vault), vault.to_owned());
                }
            }
        }
        if !paths.is_empty() {
            let known = select(paths, "bindings")?.unwrap();
            if !usable(&known.path)? {
                return Err("Previously configured Obsidian vault is unavailable".into());
            }
            return Ok(Some(known));
        }
        let Some(registry) = registry else {
            return Ok(None);
        };
        let Some(text) = io::read_limit(registry, 1_048_576)? else {
            return Ok(None);
        };
        let settings = read_json(text.as_bytes(), 1_048_576)?;
        let Some(vaults) = settings.get("vaults") else {
            return Ok(None);
        };
        for record in vaults
            .as_object()
            .ok_or("Invalid Obsidian vault registry")?
            .values()
        {
            if let Some(path) = record["path"].as_str().map(Path::new) {
                if usable(path) == Ok(true) {
                    paths.insert(io::normalized(path), path.to_owned());
                }
            }
        }
        if paths.len() > 1 {
            let managed = paths
                .iter()
                .filter(|(_, path)| self.check_home(&path.join("Codex Memory")).is_ok())
                .map(|(key, path)| (key.clone(), path.clone()))
                .collect::<BTreeMap<_, _>>();
            if managed.len() == 1 {
                return select(managed, "obsidian-memory");
            }
        }
        select(paths, "obsidian")
    }

    pub fn vault_status(&self) -> Result<Value> {
        io::profile_state(&self.state, false)?;
        Ok(match self.discover(self.registry().as_deref())? {
            Some(v) => json!({"vault":v.path,"source":v.source}),
            None => json!({"vault":null}),
        })
    }

    fn registry(&self) -> Option<PathBuf> {
        if cfg!(test) {
            Some(self.state.join("test-obsidian.json"))
        } else {
            registry_path()
        }
    }

    pub fn pin_vault(&self, vault: &Path) -> Result<Value> {
        if !usable(vault)? {
            return Err("Select an existing absolute Obsidian vault".into());
        }
        io::profile_state(&self.state, true)?;
        let path = self.state.join("vault.json");
        let _lock = self.lock_binding(&path)?;
        io::write(&path, &json!({"vault":vault}).to_string(), true, None)?;
        Ok(json!({"vault":vault,"source":"pinned"}))
    }

    pub fn auto_resolve(&self, cwd: &Path) -> Result<Option<Binding>> {
        let cwd = io::absolute(cwd)?;
        if cwd.parent().is_none() || io::within(&self.state, &cwd) {
            return Ok(None);
        }
        io::state(&self.state, &cwd, false)?;
        let identity = scope::identity(&cwd)?;
        let owner = io::hash(&identity);
        let settings = self.state.join(format!("{owner}.json"));
        if io::file(&settings)? {
            return self.read_binding(&settings);
        }
        if io::home().is_ok_and(|home| io::equal(&home, &cwd))
            || io::equal(&std::env::temp_dir(), &cwd)
            || io::within(&cwd, &self.state)
            || system_directory(&cwd)
        {
            return Ok(None);
        }
        let Some(vault) = self.discover(self.registry().as_deref())? else {
            return Ok(None);
        };
        if io::within(&cwd, &vault.path) || io::within(&vault.path, &cwd) {
            return Ok(None);
        }
        for parent in cwd.ancestors() {
            if io::directory(&parent.join(".obsidian"), false, false)? {
                return Ok(None);
            }
        }
        let _binding_lock = self.lock_binding(&settings)?;
        if io::file(&settings)? {
            return self.read_binding(&settings);
        }
        let prefix = project_prefix(&identity);
        self.setup_locked(
            &vault.path,
            &prefix,
            Some("auto"),
            &settings,
            None,
            Some(&owner),
        )
        .map(Some)
    }
}

fn system_directory(cwd: &Path) -> bool {
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot").is_some_and(|root| io::within(cwd, Path::new(&root)))
    }
    #[cfg(not(windows))]
    {
        ["/dev", "/proc", "/sys", "/etc", "/usr", "/boot", "/System"]
            .iter()
            .any(|root| io::within(cwd, Path::new(root)))
    }
}

fn project_prefix(identity: &str) -> String {
    let (_, location) = identity.split_once(':').unwrap();
    let path = Path::new(location);
    let path = if identity.starts_with("git:") && path.file_name().is_some_and(|s| s == ".git") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let slug = name
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    if slug.is_empty() {
        "project".into()
    } else {
        slug.chars()
            .take(26)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned()
    }
}

// The vault lock covers both claiming a name and initializing the project. Only
// the opaque identity digest is recorded in the vault, never workstation paths.
pub(super) fn claim_project(root: &Path, prefix: &str, owner: &str) -> Result<String> {
    for length in [12, 16, 20] {
        let name = format!("{prefix}-{}", &owner[..length]);
        let dir = root.join("Projects").join(&name);
        io::directory(&dir, true, false)?;
        let marker = dir.join(".fstack-scope");
        if io::read_limit(&marker, 256)?.as_deref() == Some(owner) {
            return Ok(name);
        }
        if fs::read_dir(&dir).map_err(io::err)?.next().is_none()
            && io::write(&marker, owner, false, None)?
        {
            return Ok(name);
        }
    }
    Err("Automatic memory project names are occupied".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &Path) -> Store {
        let package = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap();
        Store::new(Some(root.join("state")), package).unwrap()
    }

    #[test]
    fn simultaneous_projects_use_existing_vault_and_keep_independent_records() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let store = store(&root);
        let vault = root.join("notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let old = root.join("origin");
        let first = root.join("a/widget");
        let second = root.join("b/widget");
        for path in [&old, &first, &second] {
            fs::create_dir_all(path).unwrap();
        }
        let legacy = store
            .setup(&vault, &old, "hand-picked", Some("inbox"))
            .unwrap();
        let accidental = legacy.root.join("Projects").join(format!(
            "widget-{}",
            &io::hash(scope::identity(&first).unwrap())[..12]
        ));
        fs::create_dir(&accidental).unwrap();
        fs::write(accidental.join("Keep.md"), "Unmanaged project").unwrap();
        assert_eq!(store.vault_status().unwrap()["source"], "bindings");
        let work = [&first, &first, &second];
        let found = std::thread::scope(|s| {
            let pending = work.map(|cwd| s.spawn(|| store.auto_resolve(cwd).unwrap().unwrap()));
            pending.map(|worker| worker.join().unwrap())
        });
        assert_eq!(found[0].project, found[1].project);
        assert_eq!(found[0].generation, found[1].generation);
        assert_ne!(found[0].project, found[2].project);
        assert_ne!(store.root(&found[0]).unwrap(), accidental);
        assert_eq!(
            fs::read_to_string(accidental.join("Keep.md")).unwrap(),
            "Unmanaged project"
        );
        assert_eq!(found[0].mode, "auto");
        let saved = store.save(&found[0], &json!({"kind":"decision","title":"Coverage","body":"Prefer a single test runner."}), true,None,None).unwrap();
        assert!(store.get(&found[2], saved["id"].as_str().unwrap()).is_err());
        assert!(store.auto_resolve(&vault).unwrap().is_none());
        assert!(store.disable(&first).unwrap());
        assert!(store.auto_resolve(&first).unwrap().is_none());
        let restored = store
            .setup(&vault, &first, &found[0].project, None)
            .unwrap();
        assert_ne!(restored.generation, found[0].generation);
        assert!(store.get(&restored, saved["id"].as_str().unwrap()).is_ok());
        fs::rename(&vault, root.join("offline")).unwrap();
        assert_eq!(store.auto_resolve(&old).unwrap().unwrap().mode, "inbox");
    }

    #[test]
    fn uncertain_registry_never_picks_an_unrelated_vault_and_status_is_read_only() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let store = store(&root);
        let repo = root.join("project");
        fs::create_dir(&repo).unwrap();
        assert_eq!(store.vault_status().unwrap(), json!({"vault":null}));
        assert!(store.auto_resolve(&repo).unwrap().is_none());
        assert!(!store.state.exists());
        let vaults = [root.join("work"), root.join("personal")];
        for v in &vaults {
            fs::create_dir_all(v.join(".obsidian")).unwrap();
        }
        fs::create_dir(&store.state).unwrap();
        fs::write(store.registry().unwrap(), json!({"vaults":{
            "one":{"path":vaults[0]}, "two":{"path":vaults[1],"open":true}, "removed":{"path":root.join("gone")}
        }}).to_string()).unwrap();
        assert!(store.auto_resolve(&repo).is_err());
        assert!(!vaults.iter().any(|v| v.join("Codex Memory").exists()));
        let selected = store.pin_vault(&vaults[0]).unwrap();
        assert_eq!(selected["source"], "pinned");
        let b = store.auto_resolve(&repo).unwrap().unwrap();
        assert_eq!(b.root, vaults[0].join("Codex Memory"));
        let elsewhere = store.state.join("vault.json");
        fs::remove_file(elsewhere).unwrap();
        fs::remove_file(scope::binding_path(&store.state, &repo).unwrap()).unwrap();
        assert_eq!(store.vault_status().unwrap()["source"], "obsidian-memory");
        assert_eq!(
            store.auto_resolve(&repo).unwrap().unwrap().project,
            b.project
        );
        assert!(!vaults[1].join("Codex Memory").exists());
    }
}
