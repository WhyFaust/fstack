use super::{
    format, io, read_json,
    store::{Binding, Store},
    views, Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path};

const MARKER: &str = "fstack_shared_preferences: 1";
const PAGE: &str = "---\nfstack_shared_preferences: 1\n---\n\n# Общие предпочтения\n\nЗдесь хранятся только предпочтения, которые пользователь попросил применять во всех проектах этого хранилища.\n";
pub(super) const LINK: &str = "[[Codex Memory/Shared/Shared]]";

pub(super) fn is_root(root: &Path) -> bool {
    root.file_name().is_some_and(|s| s == "Shared")
        && root
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|s| s == "Codex Memory")
}

fn binding(b: &Binding) -> Binding {
    Binding {
        shared: true,
        ..b.clone()
    }
}

fn check_page(root: &Path) -> Result<bool> {
    Ok(views::marker(
        io::read(&root.join("Shared.md"))?.as_deref(),
        MARKER,
    ))
}

fn for_recall(store: &Store, project: &Binding) -> Result<Option<Binding>> {
    let shared = binding(project);
    let root = store.root(&shared)?;
    if !check_page(&root)? {
        return Ok(None);
    }
    Ok(Some(shared))
}

pub(super) fn recall_context(
    store: &Store,
    project: &Binding,
    query: &str,
    prefix: &str,
) -> Result<Option<String>> {
    const LIMIT: usize = 3;
    let Some(shared) = for_recall(store, project)? else {
        return Ok(None);
    };
    let query = query.chars().take(500).collect::<String>();
    let records = store.recall(&shared, &query, LIMIT)?;
    let ids: Vec<_> = records
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "preference")
        .filter_map(|r| r["id"].as_str())
        .collect();
    if ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("Общие предпочтения. Прочитай их через {prefix} shared show ID и примени подходящие. ID: {}. Для других условий задачи доступен shared search.", ids.join(", "))))
}

fn prepare(store: &Store, project: &Binding) -> Result<Binding> {
    let shared = binding(project);
    let root = store.root(&shared)?;
    io::directory(&root, true, false)?;
    let _lock = io::lock(&root, shared.root.parent().ok_or("Invalid memory root")?)?;
    if !check_page(&root)? {
        if fs::read_dir(&root).map_err(io::err)?.next().is_some() {
            return Err("Shared preferences directory is occupied".into());
        }
        io::write(&root.join("Shared.md"), PAGE, false, None)?;
    }
    io::directory(&root.join("Records"), true, false)?;
    Ok(shared)
}

fn scoped(mut result: Value) -> Value {
    match &mut result {
        Value::Array(entries) => {
            for entry in entries {
                entry["scope"] = json!("shared");
            }
        }
        Value::Object(_) => {
            result["scope"] = json!("shared");
        }
        _ => {}
    }
    result
}

pub(super) fn check_kind(fields: &format::Fields) -> Result<()> {
    if fields["kind"] != "preference" {
        return Err("Only an explicitly shared preference can be saved in shared memory".into());
    }
    Ok(())
}

pub(super) fn for_action(store: &Store, project: &Binding, action: &Value) -> Result<Binding> {
    if matches!(action["action"].as_str(), Some("remember" | "draft")) {
        prepare(store, project)
    } else {
        for_recall(store, project)?.ok_or("Shared preferences are unavailable".into())
    }
}

pub(super) fn remember(store: &Store, project: &Binding, candidate: &Value) -> Result<Value> {
    let fields = format::candidate(candidate)?;
    check_kind(&fields)?;
    let shared = prepare(store, project)?;
    store.save(&shared, candidate, true, None, None).map(scoped)
}

pub(super) fn execute(store: &Store, project: &Binding, args: &[String]) -> Result<Value> {
    let (command, rest) = args.split_first().ok_or("Missing shared memory command")?;
    if command == "remember" && rest.is_empty() {
        return remember(store, project, &read_json(std::io::stdin().lock(), 32768)?);
    }
    let shared = binding(project);
    let root = store.root(&shared)?;
    let exists = check_page(&root)?;
    let result = match (command.as_str(), rest) {
        ("search", [query]) => search(store, &shared, exists, query, 5),
        ("search", [query, flag, limit]) if flag == "--limit" => search(
            store,
            &shared,
            exists,
            query,
            limit.parse().map_err(io::err)?,
        ),
        ("inbox", []) if !exists => Ok(json!([])),
        ("inbox", []) => store.inbox(&shared),
        ("show" | "history" | "approve" | "forget", [id]) if !exists => {
            format::id(id)?;
            Err("Memory entry is unavailable".into())
        }
        ("show", [id]) => store.get(&shared, id),
        ("history", [id]) => store.history(&shared, id, 5),
        ("approve", [id]) => store.approve(&shared, id),
        ("forget", [id]) => store.forget(&shared, id),
        _ => Err("Invalid shared memory command arguments".into()),
    }?;
    Ok(scoped(result))
}

fn search(
    store: &Store,
    shared: &Binding,
    exists: bool,
    query: &str,
    limit: usize,
) -> Result<Value> {
    if !(1..=50).contains(&limit) || query.chars().count() > 500 {
        return Err("Invalid memory search query or limit".into());
    }
    if exists {
        store.search(shared, query, limit)
    } else {
        Ok(json!([]))
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
