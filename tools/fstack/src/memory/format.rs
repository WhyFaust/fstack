use super::{io, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub type Fields = BTreeMap<String, String>;
pub const MEMORY: &str = "fstack_memory: 1";
pub const APPROVED: &str = "fstack_approved: 1";
pub const FORGOTTEN: &str = "fstack_forgotten: 1";
pub fn matches(pattern: &str, value: &str) -> bool {
    thread_local! { static PATTERNS: std::cell::RefCell<BTreeMap<String, Regex>> = const { std::cell::RefCell::new(BTreeMap::new()) }; }
    PATTERNS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= 128 && !cache.contains_key(pattern) {
            cache.clear();
        }
        cache
            .entry(pattern.into())
            .or_insert_with(|| Regex::new(pattern).unwrap())
            .is_match(value)
    })
}
pub fn id(value: &str) -> Result<&str> {
    if matches(r"\A[0-9a-f]{64}\z", value) {
        Ok(value)
    } else {
        Err("Invalid memory entry ID".into())
    }
}
pub fn project(value: &str) -> Result<&str> {
    if !matches(r"\A[a-z0-9][a-z0-9-]{0,47}\z", value)
        || matches(r"\A(?:con|prn|aux|nul|(?:com|lpt)[0-9])\z", value)
    {
        Err("Invalid memory project slug".into())
    } else {
        Ok(value)
    }
}
pub fn safe(value: &Value, limit: usize, multiline: bool) -> Result<String> {
    let value = value.as_str().ok_or("Invalid memory text")?;
    if value.chars().count() > limit {
        return Err("Invalid memory text length".into());
    }
    let s = value.replace("\r\n", "\n").trim().to_owned();
    if s.is_empty()
        || s.chars().any(|c| {
            ((c as u32) < 32 || (127..=159).contains(&(c as u32)))
                && !(multiline && matches!(c, '\n' | '\t'))
        })
    {
        return Err("Invalid characters in memory text".into());
    }
    let secret = r#"(?i)-----BEGIN [^-\n]*PRIVATE KEY-----|\b(?:sk-[\w-]{16,}|github_pat_[\w]{16,}|gh[pousr]_[\w]{16,}|AKIA[A-Z0-9]{16}|xox[baprs]-[\w-]{12,}|Bearer\s+[\w./+=-]{8,})\b|\b(?:[\w-]*[_-])?(?:password|passwd|secret|api[_-]?key|(?:access|auth)[_-]?token|token)["']?\s*[:=]\s*["']?[^\s"']{4,}|://[^\s/@]+:[^\s/@]+@"#;
    if matches(secret, &s) || matches(r"!\[|</?[a-zA-Z][^>\n]*>|(?m:^\s*(?:```|~~~))", &s) {
        return Err(
            "Memory text contains a possible secret, embedded content or a code block".into(),
        );
    }
    Ok(s)
}
pub fn candidate(v: &Value) -> Result<Fields> {
    let m = v.as_object().ok_or("Invalid memory candidate")?;
    if m.keys().any(|k| {
        !matches!(
            k.as_str(),
            "kind" | "title" | "body" | "source" | "supersedes"
        )
    }) {
        return Err("Invalid memory candidate fields".into());
    }
    let mut out = Fields::new();
    for (k, l) in [
        ("kind", 16),
        ("title", 160),
        ("body", 4000),
        ("source", 300),
    ] {
        if k == "source" && !m.contains_key(k) {
            continue;
        }
        out.insert(
            k.into(),
            safe(m.get(k).unwrap_or(&Value::Null), l, k == "body")?,
        );
    }
    if !matches!(
        out["kind"].as_str(),
        "decision" | "preference" | "correction"
    ) {
        return Err("Invalid memory entry kind".into());
    }
    if let Some(s) = m.get("supersedes") {
        out.insert(
            "supersedes".into(),
            id(s.as_str().ok_or("Invalid predecessor")?)?.into(),
        );
    }
    Ok(out)
}
pub fn markdown(marker: &str, fields: &Fields, body: &str) -> String {
    let metadata = fields
        .iter()
        .map(|(k, v)| format!("{k}: {}", json!(v)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("---\n{marker}\n{metadata}\n---\n\n{body}\n")
}
pub fn parse(raw: &str, entry: &str, marker: &str, include_body: bool) -> Result<Fields> {
    let (meta, body) = raw
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n"))
        .ok_or("Incomplete memory metadata")?;
    if meta.lines().filter(|s| *s == marker).count() != 1 {
        return Err("Invalid memory file marker".into());
    }
    let mut fields = Fields::new();
    for line in meta.lines() {
        if line == marker {
            continue;
        }
        if marker == MEMORY
            && !include_body
            && !matches!(
                line.split(':').next().unwrap_or(""),
                "id" | "fstack_status" | "supersedes"
            )
        {
            continue;
        }
        let (k, v) = line.split_once(": ").ok_or("Invalid memory metadata")?;
        let allowed = if marker == MEMORY {
            matches!(
                k,
                "id" | "kind"
                    | "title"
                    | "created"
                    | "source"
                    | "supersedes"
                    | "session_id"
                    | "turn_id"
                    | "fstack_status"
                    | "project_link"
                    | "supersedes_link"
            )
        } else {
            matches!(k, "id" | "supersedes")
        };
        if !allowed || fields.contains_key(k) {
            return Err("Invalid memory metadata".into());
        }
        let value = if v.starts_with('"') {
            serde_json::from_str::<String>(v).map_err(io::err)?
        } else if v.starts_with('\'') && v.ends_with('\'') {
            if v.len() < 2 {
                return Err("Invalid memory metadata".into());
            }
            v[1..v.len() - 1].replace("''", "'")
        } else {
            v.into()
        };
        fields.insert(k.into(), value);
    }
    if fields.get("id").map(String::as_str) != Some(entry) {
        return Err("Memory entry ID does not match its file".into());
    }
    if let Some(s) = fields.get("supersedes") {
        if id(s)? == entry {
            return Err("Memory entry cannot supersede itself".into());
        }
    }
    if marker == MEMORY {
        if fields.get("fstack_status").is_some_and(|s| s != "approved") {
            return Err("Invalid memory approval status".into());
        }
        if let Some(link) = fields.get("project_link") {
            let r=Regex::new(r"\A\[\[Codex Memory/Projects/([a-z0-9][a-z0-9-]{0,47})/(Project|[a-z0-9][a-z0-9-]{0,47})(?:\|([a-z0-9][a-z0-9-]{0,47}))?\]\]\z").unwrap();
            let c = r.captures(link).ok_or("Invalid memory graph link")?;
            if (&c[2] != "Project" && c[1] != c[2]) || c.get(3).is_some_and(|v| v.as_str() != &c[1])
            {
                return Err("Invalid memory graph link".into());
            }
        }
        if fields.get("supersedes_link").is_some_and(|s| {
            !matches(
                r"\A\[\[Codex Memory/Projects/[a-z0-9][a-z0-9-]{0,47}/Records/[0-9a-f]{64}\]\]\z",
                s,
            )
        }) {
            return Err("Invalid memory graph link".into());
        }
        if include_body {
            safe(&json!(fields.get("title")), 160, false)?;
            if !fields
                .get("kind")
                .is_some_and(|s| matches!(s.as_str(), "decision" | "preference" | "correction"))
            {
                return Err("Invalid memory entry kind".into());
            }
            for (k, v) in &fields {
                if !matches!(
                    k.as_str(),
                    "id" | "title"
                        | "kind"
                        | "supersedes"
                        | "fstack_status"
                        | "project_link"
                        | "supersedes_link"
                ) {
                    safe(&json!(v), 300, false)?;
                }
            }
            fields.insert("body".into(), safe(&json!(body.trim()), 4000, true)?);
        }
    } else if !body.trim().is_empty() {
        return Err("Memory status marker cannot contain text".into());
    }
    Ok(fields)
}
pub fn approval(fields: &Fields) -> Fields {
    fields
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "id" | "supersedes"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
pub fn inline(fields: &Fields) -> Option<Fields> {
    (fields.get("fstack_status").map(String::as_str) == Some("approved")).then(|| approval(fields))
}
pub fn public(mut fields: Fields, approval: Option<&Fields>) -> Value {
    for k in ["fstack_status", "project_link", "supersedes_link"] {
        fields.remove(k);
    }
    if let Some(a) = approval {
        fields.remove("supersedes");
        fields.extend(a.clone());
    }
    fields.insert(
        "status".into(),
        if approval.is_some() {
            "memory"
        } else {
            "inbox"
        }
        .into(),
    );
    json!(fields)
}
pub fn stable_id(fields: &Fields) -> String {
    let text = fields
        .iter()
        .filter(|(k, _)| k.as_str() != "source")
        .map(|(k, v)| format!("{}: {}", json!(k), json!(v)))
        .collect::<Vec<_>>()
        .join(", ");
    io::hash(format!("{{{text}}}"))
}
