mod format;
mod hooks;
mod io;
mod pending;
mod scope;
mod store;
#[cfg(test)]
mod tests;
mod views;
use base64::Engine;
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON value with unique keys")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v)
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, Unique(v))) = a.next_entry::<String, Unique>()? {
                    if values.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("Duplicate JSON fields"));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(Visitor)
    }
}
fn read_json(reader: impl Read, limit: usize) -> Result<Value> {
    let mut data = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut data)
        .map_err(io::err)?;
    if data.len() > limit {
        return Err("Memory input exceeds the size limit".into());
    }
    let data = data.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&data);
    let Unique(v) = serde_json::from_slice(data).map_err(|_| "Invalid UTF-8 JSON memory input")?;
    if !v.is_object() {
        return Err("Expected a JSON object".into());
    }
    Ok(v)
}
fn action(v: &Value) -> Result<Value> {
    let m = v.as_object().ok_or("Invalid staged memory action")?;
    match v["action"].as_str() {
        Some("skip") if m.len() == 1 => Ok(json!({"action":"skip"})),
        Some(a @ ("remember" | "draft")) if m.len() == 2 && m.contains_key("candidate") => {
            Ok(json!({"action":a,"candidate":format::candidate(&v["candidate"])?}))
        }
        Some(a @ ("approve" | "forget")) if m.len() == 2 && m.contains_key("id") => Ok(
            json!({"action":a,"id":format::id(v["id"].as_str().ok_or("Invalid staged memory action")?)?}),
        ),
        _ => Err("Invalid staged memory action".into()),
    }
}
fn encode(v: &Value) -> Result<String> {
    let text = v.to_string();
    if text.len() > 20000 {
        return Err("Memory action exceeds the size limit".into());
    }
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text))
}
fn decode(payload: &str) -> Result<Value> {
    if payload.len() > 26667 || !format::matches(r"\A[A-Za-z0-9_-]+\z", payload) {
        return Err("Invalid encoded memory action".into());
    }
    let data = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(io::err)?;
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&data) != payload {
        return Err("Invalid encoded memory action".into());
    }
    action(&read_json(data.as_slice(), 20000)?)
}
fn status(b: Option<&store::Binding>) -> Value {
    match b {
        None => json!({"enabled":false}),
        Some(b) => json!({"enabled":true,"root":b.root,"project":b.project,"mode":b.mode}),
    }
}
pub fn run(arguments: &[OsString], package: &Path) -> Result<()> {
    let args = arguments
        .iter()
        .map(|s| {
            s.to_str()
                .map(str::to_owned)
                .ok_or("Memory arguments require UTF-8".into())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut args = args.as_slice();
    let state = if args.first().is_some_and(|s| s == "--state-dir") {
        if args.len() < 3 {
            return Err("--state-dir requires a path and command".into());
        }
        let s = Some(PathBuf::from(&args[1]));
        args = &args[2..];
        s
    } else {
        None
    };
    let command = args.first().ok_or("Missing memory command")?;
    let store = store::Store::new(state, package)?;
    let cwd = std::env::current_dir().map_err(io::err)?;
    let result = if command == "hook" {
        let event = args
            .get(1)
            .filter(|s| {
                matches!(
                    s.as_str(),
                    "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop"
                )
            })
            .ok_or("Invalid memory hook event")?;
        if args.len() != 2 {
            return Err("Invalid memory hook arguments".into());
        }
        match read_json(std::io::stdin().lock(), 2_097_152)
            .and_then(|v| hooks::hook(&store, event, &v, &cwd))
        {
            Ok(v) => v,
            Err(_) => json!({"systemMessage":"Хук памяти fstack не смог обработать событие."}),
        }
    } else {
        execute(&store, args, &cwd)?
    };
    println!(
        "{}",
        if command == "stage" {
            "{\"staged\": true}".into()
        } else {
            result.to_string()
        }
    );
    Ok(())
}
fn execute(store: &store::Store, args: &[String], cwd: &Path) -> Result<Value> {
    let command = args[0].as_str();
    let rest = &args[1..];
    if command == "init" {
        let mut opts = std::collections::BTreeMap::new();
        if !rest.len().is_multiple_of(2) {
            return Err("Invalid memory init arguments".into());
        }
        for pair in rest.chunks(2) {
            if !matches!(
                pair[0].as_str(),
                "--vault" | "--repo" | "--project" | "--mode"
            ) || opts.insert(pair[0].as_str(), pair[1].as_str()).is_some()
            {
                return Err("Invalid memory init arguments".into());
            }
        }
        let repo = Path::new(*opts.get("--repo").ok_or("Missing --repo")?);
        io::state(&store.state, &io::absolute(repo)?, true)?;
        let b = store.setup(
            Path::new(*opts.get("--vault").ok_or("Missing --vault")?),
            repo,
            opts.get("--project").ok_or("Missing --project")?,
            opts.get("--mode").copied(),
        )?;
        return Ok(status(Some(&b)));
    }
    if command == "pack" && rest.is_empty() {
        return Ok(json!(encode(&action(&read_json(
            std::io::stdin().lock(),
            20000
        )?)?)?));
    }
    if command == "disable" && rest.is_empty() {
        return Ok(json!({"disabled":store.disable(cwd)?}));
    }
    let b = store.resolve(cwd)?;
    if command == "status" && rest.is_empty() {
        return Ok(status(b.as_ref()));
    }
    let b = b.ok_or("Memory is not enabled for this project")?;
    match (command, rest) {
        ("search", [query]) => store.search(&b, query, 5),
        ("search", [query, flag, n]) if flag == "--limit" => {
            store.search(&b, query, n.parse().map_err(io::err)?)
        }
        ("show", [id]) => store.get(&b, id),
        ("inbox", []) => store.inbox(&b),
        ("approve", [id]) => store.approve(&b, id),
        ("forget", [id]) => store.forget(&b, id),
        ("migrate", []) => store.migrate(&b, false),
        ("migrate", [flag]) if flag == "--prune-legacy" => store.migrate(&b, true),
        ("remember", []) => store.save(
            &b,
            &read_json(std::io::stdin().lock(), 32768)?,
            true,
            None,
            None,
        ),
        ("stage", [turn_flag, token, payload_flag, payload])
            if turn_flag == "--turn" && payload_flag == "--payload" =>
        {
            decode(payload)?;
            if !pending::Pending::new(&store.state, cwd)
                .is_current(token, &hooks::scope_id(&b, cwd))?
            {
                return Err("Invalid or expired memory turn token".into());
            }
            Ok(json!({"staged":true}))
        }
        _ => Err("Invalid memory command arguments".into()),
    }
}
