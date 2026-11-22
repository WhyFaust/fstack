use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use crate::{version, Result};

pub(crate) const PACKAGE_VERSION: &str = "launchers/package-version";
const PACKAGE_MAJOR: &str = "launchers/package-major";

macro_rules! rx {
    ($pattern:expr) => {{
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("constant regex"));
        &*PATTERN
    }};
}

mod markdown;

const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".codex",
    ".aws",
    ".ssh",
    ".venv",
    "work",
    "dist",
    "target",
    "bin",
    "__pycache__",
    "node_modules",
];

pub(crate) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn json_file(path: &Path) -> Result<Value> {
    serde_json::from_str(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

fn yaml_mapping_keys(value: &serde_yaml::Value) -> Result<()> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                require(key.is_string(), "Ключи YAML должны быть строками")?;
                yaml_mapping_keys(value)?;
            }
        }
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                yaml_mapping_keys(value)?;
            }
        }
        serde_yaml::Value::Tagged(value) => yaml_mapping_keys(&value.value)?,
        _ => (),
    }
    Ok(())
}

fn yaml(text: &str) -> Result<Value> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    yaml_mapping_keys(&value)?;
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn toml_file(path: &Path) -> Result<Value> {
    let value: toml::Value =
        toml::from_str(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("{field}: ожидалась строка"))
}

fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
    }
}

pub(crate) fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|entry| entry.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

pub(crate) fn contained(
    root: &Path,
    parent: &Path,
    relative: impl AsRef<Path>,
    directory: bool,
) -> Result<PathBuf> {
    let source = parent.join(relative);
    let resolved =
        fs::canonicalize(&source).map_err(|e| format!("Нет ресурса: {}: {e}", source.display()))?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    require(
        resolved.starts_with(root) && (resolved.is_file() || directory && resolved.is_dir()),
        format!("Нет ресурса: {}", source.display()),
    )?;
    Ok(resolved)
}

fn walk(path: &Path, excluded: &[&str], result: &mut Vec<PathBuf>) -> Result<()> {
    for child in entries(path)? {
        if child
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| excluded_dir(name, excluded))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&child).map_err(|e| e.to_string())?;
        result.push(child.clone());
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            walk(&child, excluded, result)?;
        }
    }
    Ok(())
}

fn excluded_dir(name: &str, excluded: &[&str]) -> bool {
    excluded.iter().any(|entry| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(entry)
        } else {
            name == *entry
        }
    })
}

pub(crate) fn descendants(path: &Path, excluded: &[&str]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    walk(path, excluded, &mut paths)?;
    Ok(paths)
}

fn extension_matches(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            if cfg!(windows) {
                value.eq_ignore_ascii_case(expected)
            } else {
                value == expected
            }
        })
}

fn skill_markdown(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            if cfg!(windows) {
                let value = value.to_ascii_lowercase();
                value.starts_with("skill") && value.ends_with(".md")
            } else {
                value.starts_with("SKILL") && value.ends_with(".md")
            }
        })
}

fn parse_skill<'a>(path: &Path, text: &'a str) -> Result<(Value, &'a str)> {
    let header = rx!(r"(?m)\A---\r?\n((?s:.*?))^---\r?$")
        .captures(text)
        .ok_or("Отсутствует frontmatter")?;
    let data = yaml(&header[1])?;
    require(data.is_object(), "Frontmatter должен быть объектом")?;
    let name = string(&data, "name")?;
    require(valid_skill_name(name), "Некорректное имя навыка")?;
    require(
        path.parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            == Some(name),
        "Имя отличается от каталога",
    )?;
    let description = string(&data, "description")?;
    require(
        !description.trim().is_empty() && description.chars().count() <= 1024,
        "description должен содержать 1–1024 символа",
    )?;
    for field in ["license", "compatibility", "allowed-tools"] {
        if let Some(value) = data.get(field) {
            require(
                nonempty(value),
                format!("{field} должен быть непустой строкой"),
            )?;
        }
    }
    if let Some(value) = data.get("compatibility") {
        require(
            value.as_str().is_some_and(|v| v.chars().count() <= 500),
            "compatibility длиннее 500 символов",
        )?;
    }
    if let Some(value) = data.get("metadata") {
        require(
            value
                .as_object()
                .is_some_and(|fields| fields.values().all(Value::is_string)),
            "metadata должен быть строковым словарём",
        )?;
    }
    if let Some(value) = data.get("disable-model-invocation") {
        require(
            value.is_boolean(),
            "disable-model-invocation должен быть булевым значением",
        )?;
    }
    Ok((data, &text[header.get(0).unwrap().end()..]))
}

pub(crate) fn metadata(path: &Path) -> Result<Value> {
    let (data, _) = parse_skill(path, &read(path)?)?;
    Ok(data)
}

pub(crate) fn valid_skill_name(name: &str) -> bool {
    name.len() <= 64 && rx!(r"\A[a-z0-9]+(?:-[a-z0-9]+)*\z").is_match(name)
}

fn metadata_text(value: &Value, field: &str, max: usize) -> Result<String> {
    let text = string(value, field)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    require(
        !text.is_empty() && text.chars().count() <= max,
        format!("{field}: ожидалось 1–{max} символов"),
    )?;
    Ok(text)
}

fn lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        if part == Component::ParentDir {
            normalized.pop();
        } else if part != Component::CurDir {
            normalized.push(part.as_os_str());
        }
    }
    normalized
}

fn skill_ui_file(root: &Path, folder: &Path, name: &str, path: &Path) -> Result<Value> {
    let ui = yaml(&read(path)?)?;
    require(ui.is_object(), "Метаданные UI должны быть объектом")?;
    for field in ["interface", "policy", "dependencies"] {
        require(
            ui[field].is_null() || ui[field].is_object(),
            format!("{field}: ожидался объект"),
        )?;
    }
    let interface = &ui["interface"];
    if interface.get("display_name").is_some() {
        metadata_text(interface, "display_name", 64)?;
    }
    if interface.get("short_description").is_some() {
        let description = metadata_text(interface, "short_description", 64)?;
        require(description.chars().count() >= 25, "Короткое UI description")?;
    }
    if interface.get("default_prompt").is_some() {
        let prompt = metadata_text(interface, "default_prompt", 1024)?;
        require(
            rx!(r"\$fstack:([a-z0-9-]+)")
                .captures_iter(&prompt)
                .any(|entry| &entry[1] == name),
            "default_prompt без полного имени навыка",
        )?;
    }
    if let Some(value) = ui["policy"].get("allow_implicit_invocation") {
        require(
            value.is_null() || value.is_boolean(),
            "Некорректная доступность",
        )?;
    }
    if let Some(value) = interface.get("brand_color").filter(|v| !v.is_null()) {
        require(
            value
                .as_str()
                .is_some_and(|v| rx!(r"\A#[0-9a-fA-F]{6}\z").is_match(v.trim())),
            "brand_color: ожидался #RRGGBB",
        )?;
    }
    for field in ["icon_small", "icon_large"] {
        if let Some(value) = interface.get(field).filter(|v| !v.is_null()) {
            let relative = Path::new(value.as_str().ok_or("Путь иконки должен быть строкой")?);
            require(
                !relative.is_absolute(),
                "Путь иконки должен быть относительным",
            )?;
            let shared = relative
                .components()
                .any(|part| part == Component::ParentDir);
            if !shared {
                require(
                    relative
                        .components()
                        .find(|part| *part != Component::CurDir)
                        == Some(Component::Normal("assets".as_ref())),
                    "Иконка должна находиться в assets/",
                )?;
            }
            let assets = if shared { root } else { folder }.join("assets");
            if shared {
                let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                require(
                    lexical_path(&cwd.join(folder).join(relative))
                        .starts_with(lexical_path(&cwd.join(&assets))),
                    "Общий путь иконки должен находиться в assets/ пакета",
                )?;
            }
            contained(&assets, folder, relative, false)?;
        }
    }
    if let Some(tools) = ui["dependencies"].get("tools") {
        for tool in tools
            .as_array()
            .ok_or("dependencies.tools: ожидался список")?
        {
            let kind = metadata_text(tool, "type", 64)?;
            require(
                kind.eq_ignore_ascii_case("mcp"),
                "Поддерживается зависимость mcp",
            )?;
            metadata_text(tool, "value", 1024)?;
            for (field, max) in [
                ("description", 1024),
                ("transport", 64),
                ("url", 1024),
                ("command", 1024),
            ] {
                if tool.get(field).is_some_and(|v| !v.is_null()) {
                    metadata_text(tool, field, max)?;
                }
            }
        }
    }
    Ok(ui)
}

fn skill_ui(root: &Path, folder: &Path, name: &str) -> Result<()> {
    let agents = folder.join("agents");
    let canonical = agents.join("openai.yaml");
    let localized = entries(&agents)?.into_iter().filter(|path| {
        path != &canonical
            && extension_matches(path, "yaml")
            && path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("openai")
                    || (cfg!(windows) && name.to_ascii_lowercase().starts_with("openai"))
            })
    });
    for path in std::iter::once(canonical.clone()).chain(localized) {
        let ui = skill_ui_file(root, folder, name, &path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        require(
            ui["policy"]["allow_implicit_invocation"] == false,
            format!("{}: навыки fstack требуют явного включения", path.display()),
        )?;
    }
    Ok(())
}

fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let a = (bytes[index + 1] as char).to_digit(16);
            let b = (bytes[index + 2] as char).to_digit(16);
            if let (Some(a), Some(b)) = (a, b) {
                result.push((a * 16 + b) as u8);
                index += 3;
                continue;
            }
        }
        result.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn resource_directories(folder: &Path, root: &Path) -> Result<()> {
    for name in ["scripts", "references", "assets"] {
        let directory = folder.join(name);
        if directory.exists() {
            contained(root, folder, name, true)?;
            require(directory.is_dir(), "Ожидался каталог ресурсов")?;
            let mut content = false;
            for path in descendants(&directory, EXCLUDED_DIRECTORIES)? {
                let resource = contained(
                    root,
                    &directory,
                    path.strip_prefix(&directory).map_err(|e| e.to_string())?,
                    true,
                )?;
                if resource.is_file()
                    && resource.metadata().map_err(|e| e.to_string())?.len() > 0
                    && ![Some(".gitkeep"), Some(".gitignore")]
                        .contains(&path.file_name().and_then(|n| n.to_str()))
                {
                    content = true;
                }
            }
            require(
                content,
                format!("{}: пустой каталог ресурсов", directory.display()),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn resources(folder: &Path, root: &Path) -> Result<()> {
    let directory = contained(root, folder, ".", true)?;
    resource_directories(folder, root)?;
    let mut links = markdown::Links::new(root)?;
    for path in descendants(&directory, EXCLUDED_DIRECTORIES)? {
        if markdown::is_markdown(&path) {
            links.check(&path, &links.read_document(&path)?)?;
        }
    }
    Ok(())
}

pub(crate) fn check_launcher_version(root: &Path, number: &str) -> Result<()> {
    let path = contained(root, root, PACKAGE_VERSION, false)?;
    if fs::read(path).map_err(|e| e.to_string())? != format!("{number}\n").as_bytes() {
        return Err(format!(
            "Версия запускателя {PACKAGE_VERSION}: ожидается {number}; выполните release prepare"
        ));
    }
    check_package_major(root, number)
}

pub(crate) fn check_package_major(root: &Path, number: &str) -> Result<()> {
    let major = version::parse(number)?[0];
    let policy = read(&contained(root, root, PACKAGE_MAJOR, false)?)?;
    require(
        policy.lines().eq([major.to_string()]),
        format!("Версия требует явного изменения {PACKAGE_MAJOR}"),
    )
}

pub(crate) fn check_versions(root: &Path, number: &str) -> Result<()> {
    version::parse(number)?;
    check_launcher_version(root, number)
}

fn hooks(root: &Path, relative: &str) -> Result<()> {
    let config = json_file(&contained(root, root, relative, false)?)?;
    let hooks = config["hooks"]
        .as_object()
        .ok_or("Неверный набор событий координатора")?;
    let expected = ["SessionStart", "SubagentStart", "UserPromptSubmit"];
    require(
        hooks.len() == expected.len() && expected.iter().all(|e| hooks.contains_key(*e)),
        "Неверный набор событий координатора",
    )?;
    contained(root, root, "tools/fstack/src/coordinator", true)?;
    contained(root, root, "launchers/fstack.sh", false)?;
    contained(root, root, "launchers/fstack.ps1", false)?;
    contained(root, root, "hooks/coordinator.sh", false)?;
    contained(root, root, "hooks/coordinator.ps1", false)?;
    contained(root, root, "hooks/runtime-unavailable.json", false)?;
    let runtime_id = read(&contained(root, root, "launchers/runtime-id", false)?)?;
    require(
        rx!(r"\A[0-9a-f]{64}\n\z").is_match(&runtime_id),
        "Некорректный идентификатор сборки",
    )?;
    for (event, groups) in hooks {
        let groups = groups.as_array().ok_or("Ожидался массив обработчиков")?;
        require(groups.len() == 1, "Ожидалась одна группа хуков")?;
        let handlers = groups[0]["hooks"].as_array().ok_or("Нет команд хуков")?;
        let session_start = event == "SessionStart";
        require(
            handlers.len() == 1 + usize::from(session_start),
            format!("{event}: неверное число команд хуков"),
        )?;
        require(
            groups[0].get("matcher").is_none(),
            format!("{event}: неверный фильтр хука"),
        )?;
        for (index, handler) in handlers.iter().enumerate() {
            let bootstrap = session_start && index == 0;
            let event = if bootstrap { "Bootstrap" } else { event };
            check_hook_command(event, handler)?;
        }
    }
    Ok(())
}

fn check_hook_command(event: &str, handler: &Value) -> Result<()> {
    if matches!(event, "SessionStart" | "UserPromptSubmit") {
        require(
            handler["additionalContextLimit"].as_u64() == Some(0),
            format!(
                "{event}: additionalContextLimit должен сохранять полный контекст хука в запросе"
            ),
        )?;
    }
    let timeout = match event {
        "Bootstrap" => 420,
        "SessionStart" => 10,
        _ => 5,
    };
    require(
        handler.get("async").map_or(Some(false), Value::as_bool) == Some(event == "Bootstrap")
            && handler["timeout"] == timeout,
        format!("{event}: неверный режим запуска хука"),
    )?;
    require(handler["type"] == "command", "Ожидался командный хук")?;
    let posix = format!(r#"/bin/sh "$PLUGIN_ROOT/hooks/coordinator.sh" {event}"#);
    let windows = format!(
        r#"\\.\GLOBALROOT\SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo -NoProfile -NonInteractive -Command "& ([scriptblock]::Create([IO.File]::ReadAllText([IO.Path]::Combine([Environment]::GetEnvironmentVariable('PLUGIN_ROOT'), 'hooks', 'coordinator.ps1')))) {event} $([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([Environment]::CurrentDirectory)))""#
    );
    require(
        string(handler, "command")? == posix,
        format!("{event}: неверная команда command"),
    )?;
    require(
        string(handler, "commandWindows")? == windows,
        format!("{event}: неверная команда commandWindows"),
    )
}

pub(crate) fn skill_names(root: &Path) -> Result<BTreeSet<String>> {
    entries(&root.join("skills"))?
        .iter()
        .filter(|p| p.is_dir())
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or_else(|| "Некорректное имя каталога".into())
        })
        .collect()
}

fn catalog_rows(listing: &str) -> Result<Vec<(String, String)>> {
    let mut rows = Vec::new();
    for row in rx!(r"(?m)^\| ([^|\n]+) \|([^\n]*)").captures_iter(listing) {
        let first = &row[1];
        let (name, target) =
            if let Some(link) = rx!(r"^\[([a-z0-9-]+)\]\(([^)]+)\)$").captures(first) {
                (link[1].to_owned(), link[2].to_owned())
            } else if let Some(name) = first.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
                let instructions = row[2]
                    .trim_end()
                    .trim_end_matches('|')
                    .rsplit('|')
                    .next()
                    .unwrap_or("");
                (
                    name.to_owned(),
                    markdown::targets(instructions)
                        .into_iter()
                        .next()
                        .unwrap_or_default(),
                )
            } else {
                continue;
            };
        let entry = format!("../skills/{name}/SKILL.md");
        require(
            target == entry,
            format!("Каталог: основная ссылка {name} должна вести на {entry}"),
        )?;
        let signal = row[2].split('|').next().unwrap_or("").trim();
        let signal = rx!(r",? \[[^\]]+\]\([^)]+\)$").replace(signal, "");
        require(!signal.is_empty(), format!("Каталог: нет описания {name}"))?;
        rows.push((name, signal.into_owned()));
    }
    Ok(rows)
}

pub(crate) fn skill_index(package: &Path) -> Result<String> {
    let catalog = contained(package, package, "docs/skills.md", false)?;
    let rows = catalog_rows(&read(&catalog)?)?;
    require(!rows.is_empty(), "Каталог навыков пуст")?;
    Ok(rows
        .into_iter()
        .map(|(name, signal)| format!("{name} — {signal}"))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn check_catalog(listing: &str, skills: &BTreeSet<String>) -> Result<()> {
    let mut listed = BTreeSet::new();
    for (name, _) in catalog_rows(listing)? {
        require(
            listed.insert(name.clone()),
            format!("Каталог: повтор {name}"),
        )?;
    }
    require(
        !skills.is_empty() && &listed == skills,
        "Каталог навыков не совпадает с пакетом",
    )
}

pub(crate) fn manifest_assets(manifest: &Value) -> Result<Vec<&str>> {
    let interface = &manifest["interface"];
    let mut assets = vec![
        string(interface, "composerIcon")?,
        string(interface, "logo")?,
    ];
    if let Some(screenshots) = interface.get("screenshots") {
        for value in screenshots
            .as_array()
            .ok_or("screenshots должен быть массивом")?
        {
            assets.push(value.as_str().ok_or("Некорректный screenshot")?);
        }
    }
    Ok(assets)
}

pub(crate) fn check(root: &Path) -> Result<String> {
    native_notices(root)?;
    let manifest = json_file(&root.join(".codex-plugin/plugin.json"))?;
    require(manifest["name"] == "fstack", "Некорректное имя плагина")?;
    check_versions(root, string(&manifest, "version")?)?;
    require(
        manifest["skills"] == "./skills/",
        "Некорректный каталог навыков",
    )?;
    require(
        manifest["extensions"]["com.openai"]
            .get("onboardingSkill")
            .is_none(),
        "Автоматический пакет не объявляет onboardingSkill",
    )?;
    require(!truthy(&manifest["mcpServers"]), "Неожиданная интеграция")?;
    hooks(root, string(&manifest, "hooks")?)?;
    let catalog = json_file(&root.join(".agents/plugins/marketplace.json"))?;
    require(
        catalog["name"] == "fstack" && catalog["plugins"].as_array().is_some_and(|v| v.len() == 1),
        "Некорректный marketplace",
    )?;
    let entry = &catalog["plugins"][0];
    require(
        entry["name"] == "fstack" && entry["source"] == json!({"source":"local","path":"./"}),
        "Некорректный source",
    )?;
    let skills = skill_names(root)?;
    let mut links = markdown::Links::new(root)?;
    let listing = links.read_document(&root.join("docs/skills.md"))?;
    check_catalog(&listing, &skills)?;
    for name in &skills {
        let folder = root.join("skills").join(name);
        let mut versions = Vec::new();
        for path in entries(&folder)? {
            if skill_markdown(&path) {
                let text = links.read_document(&path)?;
                let (data, body) = parse_skill(&path, &text)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                versions.push(
                    data["metadata"]["version"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                );
                require(
                    !body.trim().is_empty(),
                    format!("{}: отсутствует текст инструкции", path.display()),
                )?;
                require(
                    data["disable-model-invocation"] == true,
                    format!(
                        "{}: disable-model-invocation должен быть true",
                        path.display()
                    ),
                )?;
            }
        }
        require(
            folder.join("SKILL.md").is_file(),
            format!("{name}: отсутствует SKILL.md"),
        )?;
        if versions.len() > 1 {
            require(
                !versions[0].trim().is_empty() && versions.iter().all(|v| v == &versions[0]),
                format!("{name}: metadata.version должна совпадать во всех переводах"),
            )?;
        }
        resource_directories(&folder, root)?;
        skill_ui(root, &folder, name)?;
    }
    for asset in manifest_assets(&manifest)? {
        let path = contained(root, root, asset, false)?;
        require(
            fs::read(&path)
                .map_err(|e| e.to_string())?
                .starts_with(b"\x89PNG\r\n\x1a\n"),
            "Ожидался PNG",
        )?;
    }
    for relative in [
        "profile/agents",
        "profile/config.example.toml",
        ".codex/agents",
        "agents",
    ] {
        require(
            !root.join(relative).exists(),
            format!("Пакет не устанавливает глобальные роли или настройки: {relative}"),
        )?;
    }
    let mut documents: Vec<_> = entries(root)?
        .into_iter()
        .filter(|path| {
            !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    for directory in [
        "docs",
        "skills",
        ".github",
        "assets",
        "tools",
        "hooks",
        "launchers",
        "licenses",
        ".codex-plugin",
        ".agents/plugins",
    ] {
        if root.join(directory).is_dir() {
            let directory = contained(root, root, directory, true)?;
            documents.extend(descendants(&directory, EXCLUDED_DIRECTORIES)?);
        }
    }
    for path in documents {
        if markdown::is_markdown(&path) {
            let text = links.read_document(&path)?;
            for reference in rx!(r"\bfstack:([a-z][a-z0-9-]*)").captures_iter(&text) {
                let name = &reference[1];
                require(
                    skills.contains(name),
                    format!("{}: отсутствует навык {name}", path.display()),
                )?;
            }
            links.check(&path, &text)?;
        }
    }
    Ok(format!(
        "Пакет {}: {} навыков, метаданные, ссылки и ресурсы проверены.",
        string(&manifest, "version")?,
        skills.len()
    ))
}

fn native_notices(root: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let directory = match fs::symlink_metadata(root.join("tools/fstack/native-runtime.json")) {
        Ok(_) => "tools/fstack",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "licenses",
        Err(error) => return Err(error.to_string()),
    };
    let inventory = json_file(&contained(
        root,
        &root.join(directory),
        "native-runtime.json",
        false,
    )?)?;
    require(
        inventory["schema"] == 1,
        "Неверная схема уведомлений runtime",
    )?;
    for (key, relative) in [
        ("cargo_lock_sha256", "tools/fstack/Cargo.lock"),
        ("cargo_manifest_sha256", "tools/fstack/Cargo.toml"),
    ] {
        let bytes = fs::read(root.join(relative)).map_err(|e| e.to_string())?;
        require(
            inventory[key] == format!("{:x}", Sha256::digest(bytes)),
            format!("Изменён {relative}; пересмотрите лицензии и обновите inventory runtime"),
        )?;
    }
    require(
        inventory["rust_version"] == "1.90.0",
        "Пересмотрите уведомления стандартной библиотеки Rust",
    )?;
    let targets: BTreeSet<_> = inventory["targets"]
        .as_array()
        .ok_or("Нет платформ inventory")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    require(
        targets
            == [
                "x86_64-unknown-linux-gnu",
                "x86_64-pc-windows-msvc",
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
            ]
            .into_iter()
            .collect(),
        "Платформы inventory не совпадают с выпуском",
    )?;
    let files = inventory["notice_files"]
        .as_object()
        .ok_or("Нет файлов уведомлений")?;
    require(
        files.len() == 2
            && files.contains_key(&format!("{directory}/native-runtime.txt"))
            && files.contains_key(&format!("{directory}/native-runtime-rust-1.90.0.html")),
        "Неполный комплект уведомлений runtime",
    )?;
    for (relative, digest) in files {
        let bytes = fs::read(contained(root, root, relative, false)?).map_err(|e| e.to_string())?;
        require(
            !bytes.is_empty() && digest == &format!("{:x}", Sha256::digest(bytes)),
            format!("Изменено уведомление runtime: {relative}"),
        )?;
    }
    let lock = toml_file(&root.join("tools/fstack/Cargo.lock"))?;
    let locked = lock["package"].as_array().ok_or("Нет пакетов Cargo.lock")?;
    let packages = inventory["packages"]
        .as_array()
        .ok_or("Нет зависимостей inventory")?;
    require(!packages.is_empty(), "Пустой inventory runtime")?;
    let excluded = inventory["excluded_packages"]
        .as_array()
        .ok_or("Нет перечня исключённых зависимостей inventory")?;
    let mut seen = HashSet::new();
    for package in packages.iter().chain(excluded) {
        let name = string(package, "name")?;
        let version = string(package, "version")?;
        let source = string(package, "source")?;
        require(
            source.starts_with("registry+"),
            "Неподдерживаемый источник зависимости inventory",
        )?;
        require(
            seen.insert((name, version, source)),
            "Повтор зависимости inventory",
        )?;
        require(
            !packages.contains(package)
                || nonempty(&package["license"]) && nonempty(&package["notice_source"]),
            "Нет источника лицензии runtime",
        )?;
        require(
            locked.iter().any(|p| {
                p["name"] == name
                    && p["version"] == version
                    && p["source"] == package["source"]
                    && p["checksum"] == package["checksum"]
            }),
            format!("Зависимость inventory не совпадает с lock: {name} {version}"),
        )?;
    }
    require(
        seen.len()
            == locked
                .iter()
                .filter(|p| {
                    p["source"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("registry+"))
                })
                .count(),
        "Inventory не покрывает точный набор registry-зависимостей Cargo.lock",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
