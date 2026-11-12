use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

type Result<T> = std::result::Result<T, String>;

macro_rules! rx {
    ($pattern:expr) => {{
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("constant regex"));
        &*PATTERN
    }};
}

const ROLES: [&str; 5] = ["auditor", "coder", "reviewer", "verifier", "allure_analyst"];
const EFFORTS: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "ultra"];

fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn json_file(path: &Path) -> Result<Value> {
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

fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|entry| entry.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

fn contained(
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
            .is_some_and(|n| excluded.contains(&n))
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

fn descendants(path: &Path, excluded: &[&str]) -> Result<Vec<PathBuf>> {
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

fn frontmatter(path: &Path) -> Result<Value> {
    let text = read(path)?;
    let mut parts = text.splitn(3, "---");
    require(
        parts.next().is_some_and(|s| s.trim().is_empty()),
        "Отсутствует frontmatter",
    )?;
    let body = parts.next().ok_or("Отсутствует frontmatter")?;
    require(parts.next().is_some(), "Отсутствует frontmatter")?;
    let value = yaml(body)?;
    require(value.is_object(), "Frontmatter должен быть объектом")?;
    Ok(value)
}

fn metadata(path: &Path) -> Result<Value> {
    let data = frontmatter(path)?;
    let name = string(&data, "name")?;
    require(
        rx!(r"\A[a-z0-9]+(?:-[a-z0-9]+)*\z").is_match(name) && name.len() <= 64,
        "Некорректное имя навыка",
    )?;
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
        if field == "license" || data.get(field).is_some() {
            require(
                nonempty(&data[field]),
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
    let fields = data["metadata"]
        .as_object()
        .ok_or("metadata должен быть строковым словарём")?;
    require(
        fields.values().all(Value::is_string),
        "metadata должен быть строковым словарём",
    )?;
    require(
        data["metadata"]["author"] == "WhyFaust"
            && data["metadata"]["source"] == "https://github.com/WhyFaust/fstack",
        "Отсутствуют автор или источник",
    )?;
    Ok(data)
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

fn skill_ui_file(root: &Path, folder: &Path, name: &str, path: &Path) -> Result<()> {
    let ui = yaml(&read(path)?)?;
    require(ui.is_object(), "Метаданные UI должны быть объектом")?;
    for field in ["interface", "policy", "dependencies"] {
        require(
            ui[field].is_null() || ui[field].is_object(),
            format!("{field}: ожидался объект"),
        )?;
    }
    let interface = &ui["interface"];
    metadata_text(interface, "display_name", 64)?;
    let description = metadata_text(interface, "short_description", 64)?;
    require(description.chars().count() >= 25, "Короткое UI description")?;
    let prompt = metadata_text(interface, "default_prompt", 1024)?;
    require(
        prompt.contains(&format!("$fstack:{name}")),
        "default_prompt без полного имени навыка",
    )?;
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
    Ok(())
}

fn skill_ui(root: &Path, folder: &Path, name: &str) -> Result<()> {
    let agents = folder.join("agents");
    let canonical = agents.join("openai.yaml");
    skill_ui_file(root, folder, name, &canonical)
        .map_err(|error| format!("{}: {error}", canonical.display()))?;
    for path in entries(&agents)? {
        let file = path.file_name().unwrap().to_string_lossy();
        let file = if cfg!(windows) {
            file.to_ascii_lowercase()
        } else {
            file.into_owned()
        };
        if file.starts_with("openai_") && file.ends_with(".yaml") {
            skill_ui_file(root, folder, name, &path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn skill_structure(path: &Path) -> Result<()> {
    let text = read(path)?;
    let body = text
        .splitn(3, "---")
        .nth(2)
        .ok_or("Отсутствует frontmatter")?;
    let english = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case("SKILL_en.md")
            } else {
                name == "SKILL_en.md"
            }
        });
    let expected = if english {
        ["Inputs and scope", "Procedure", "Verification and result"]
    } else {
        ["Вход и границы", "Порядок работы", "Проверка и результат"]
    };
    let mut fence: Option<String> = None;
    let mut title = false;
    let mut sections = Vec::new();
    let mut content = Vec::new();
    for line in body.lines() {
        if let Some(marker) = rx!(r"^ {0,3}(`{3,}|~{3,})").captures(line) {
            let token = &marker[1];
            match &fence {
                None => fence = Some(token.to_string()),
                Some(open)
                    if open.as_bytes()[0] == token.as_bytes()[0] && token.len() >= open.len() =>
                {
                    fence = None
                }
                _ => (),
            }
            continue;
        }
        if fence.is_some() {
            if !line.trim().is_empty() {
                if let Some(present) = content.last_mut() {
                    *present = true;
                }
            }
            continue;
        }
        if let Some(heading) = rx!(r"^ {0,3}(#{1,2})(?:[ \t]+(.*)|$)").captures(line) {
            let name = heading.get(2).map_or("", |m| m.as_str()).trim();
            let name = rx!(r"[ \t]+#+[ \t]*$").replace(name, "");
            if heading[1].len() == 1 {
                require(
                    !title && sections.is_empty() && !name.is_empty(),
                    "Ожидался один заголовок H1 перед разделами",
                )?;
                title = true;
            } else {
                require(title, "Перед разделами требуется H1")?;
                sections.push(name.into_owned());
                content.push(false);
            }
        } else if !line.trim().is_empty() {
            if let Some(present) = content.last_mut() {
                *present = true;
            }
        }
    }
    require(
        title && sections == expected && content.iter().all(|present| *present),
        format!(
            "{}: требуется H1 и три непустых раздела шаблона",
            path.display()
        ),
    )
}

fn strip_inline(line: &str) -> String {
    let mut rest = line;
    let mut output = String::new();
    while let Some(start) = rest.find('`') {
        output.push_str(&rest[..start]);
        let ticks = rest[start..].bytes().take_while(|b| *b == b'`').count();
        let matched = (1..=ticks).rev().find_map(|size| {
            rest[start + size..]
                .find(&rest[start..start + size])
                .map(|end| start + size + end + size)
        });
        if let Some(end) = matched {
            rest = &rest[end..];
        } else {
            output.push_str(&rest[start..start + ticks]);
            rest = &rest[start + ticks..];
        }
    }
    output.push_str(rest);
    output
}

fn markdown_targets(text: &str) -> Vec<String> {
    let mut fence: Option<String> = None;
    let mut targets = Vec::new();
    for line in text.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{2028}',
        '\u{2029}',
    ]) {
        if let Some(marker) = rx!(r"^ {0,3}(`{3,}|~{3,})").captures(line) {
            let token = &marker[1];
            match &fence {
                None => fence = Some(token.to_string()),
                Some(open)
                    if open.as_bytes()[0] == token.as_bytes()[0] && token.len() >= open.len() =>
                {
                    fence = None
                }
                _ => (),
            }
            continue;
        }
        if fence.is_none() {
            let prose = strip_inline(line);
            targets.extend(
                rx!(r"\]\(([^)]+)\)")
                    .captures_iter(&prose)
                    .map(|c| c[1].to_string()),
            );
        }
    }
    targets
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

fn links(path: &Path, root: &Path, text: &str) -> Result<()> {
    for target in markdown_targets(text) {
        let target = target.split('#').next().unwrap_or("");
        if !target.is_empty() && !target.contains("://") && !target.starts_with("mailto:") {
            contained(
                root,
                path.parent().ok_or("Нет родительского каталога")?,
                unquote(target),
                true,
            )?;
        }
    }
    Ok(())
}

fn resources(folder: &Path, root: &Path) -> Result<()> {
    for name in ["scripts", "references", "assets"] {
        let directory = folder.join(name);
        if directory.exists() {
            contained(root, folder, name, true)?;
            require(directory.is_dir(), "Ожидался каталог ресурсов")?;
            let mut content = false;
            for path in descendants(&directory, &[])? {
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
    for path in descendants(folder, &[])? {
        if extension_matches(&path, "md") {
            links(&path, root, &read(&path)?)?;
        }
    }
    Ok(())
}

fn role_settings(role: &Value, name: &str) -> Result<()> {
    require(ROLES.contains(&name), "Неизвестная роль")?;
    require(
        nonempty(&role["model"]),
        format!("{name}: некорректная модель роли"),
    )?;
    require(
        role["model_reasoning_effort"]
            .as_str()
            .is_some_and(|e| EFFORTS.contains(&e)),
        format!("{name}: некорректный reasoning роли"),
    )
}

fn versions(root: &Path, manifest: &Value) -> Result<()> {
    let pattern = rx!(r"\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z");
    require(
        pattern.is_match(string(manifest, "version")?),
        "Некорректная версия пакета",
    )?;
    let mut count = 0;
    for folder in entries(&root.join("skills"))? {
        if !folder.is_dir() {
            continue;
        }
        let mut previous: Option<String> = None;
        for path in entries(&folder)? {
            if skill_markdown(&path) {
                let text = read(&path)?.replace("\r\n", "\n");
                require(
                    rx!(r"\A---\n(?s:.*?)\n---(?:\n|\z)").is_match(&text),
                    "Нет frontmatter версии",
                )?;
                let data = frontmatter(&path)?;
                let version = string(&data["metadata"], "version")?;
                require(pattern.is_match(version), "Некорректная версия навыка")?;
                require(
                    previous.as_deref().is_none_or(|v| v == version),
                    "Версии локализаций навыка различаются",
                )?;
                previous = Some(version.into());
                count += 1;
            }
        }
    }
    require(count > 0, "Не найдены навыки")
}

fn hooks(root: &Path, relative: &str) -> Result<()> {
    let config = json_file(&contained(root, root, relative, false)?)?;
    let hooks = config["hooks"]
        .as_object()
        .ok_or("Неверный набор событий памяти")?;
    let expected = ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];
    require(
        hooks.len() == expected.len() && expected.iter().all(|e| hooks.contains_key(*e)),
        "Неверный набор событий памяти",
    )?;
    contained(root, root, "tools/fstack/src/memory", true)?;
    for (event, groups) in hooks {
        let groups = groups.as_array().ok_or("Ожидался массив обработчиков")?;
        require(
            groups.len() == 1 && groups[0]["hooks"].as_array().is_some_and(|h| h.len() == 1),
            "Ожидалась одна команда памяти",
        )?;
        if event == "PostToolUse" {
            require(groups[0]["matcher"] == "^Bash$", "Ожидался фильтр оболочки")?;
        }
        let handler = &groups[0]["hooks"][0];
        require(handler["type"] == "command", "Ожидался командный хук")?;
        let posix = format!(r#"/bin/sh "$PLUGIN_ROOT/hooks/memory.sh" {event}"#);
        let windows = format!(
            r#"\\.\GLOBALROOT\SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo -NoProfile -NonInteractive -Command "& ([scriptblock]::Create([IO.File]::ReadAllText([IO.Path]::Combine([Environment]::GetEnvironmentVariable('PLUGIN_ROOT'), 'hooks', 'memory.ps1')))) {event} $([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([Environment]::CurrentDirectory)))""#
        );
        require(
            string(handler, "command")? == posix,
            format!("{event}: неверная команда command"),
        )?;
        require(
            string(handler, "commandWindows")? == windows,
            format!("{event}: неверная команда commandWindows"),
        )?;
        contained(root, root, "hooks/memory.sh", false)?;
        contained(root, root, "hooks/memory.ps1", false)?;
    }
    Ok(())
}

fn skill_names(root: &Path) -> Result<BTreeSet<String>> {
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

fn references(
    text: &str,
    skills: &BTreeSet<String>,
    pattern: &Regex,
    exclude_profile: bool,
) -> Result<()> {
    for capture in pattern.captures_iter(text) {
        let name = &capture[1];
        require(
            exclude_profile && name == "profile" || skills.contains(name),
            format!("Отсутствует навык: {name}"),
        )?;
    }
    Ok(())
}

fn check_catalog(listing: &str, skills: &BTreeSet<String>) -> Result<()> {
    let mut listed = BTreeSet::new();
    for row in rx!(r"(?m)^\| `([a-z0-9-]+)` \|([^\n]*)").captures_iter(listing) {
        let name = &row[1];
        let instructions = row[2]
            .trim_end()
            .trim_end_matches('|')
            .rsplit('|')
            .next()
            .unwrap_or("");
        let targets = markdown_targets(instructions);
        let entry = format!("../skills/{name}/SKILL.md");
        require(
            targets.first() == Some(&entry),
            format!("Каталог: основная ссылка {name} должна вести на {entry}"),
        )?;
        require(
            listed.insert(name.to_owned()),
            format!("Каталог: повтор {name}"),
        )?;
    }
    require(
        !skills.is_empty() && &listed == skills,
        "Каталог навыков не совпадает с пакетом",
    )
}

pub(crate) fn check(root: &Path) -> Result<String> {
    native_notices(root)?;
    let manifest = json_file(&root.join(".codex-plugin/plugin.json"))?;
    require(manifest["name"] == "fstack", "Некорректное имя плагина")?;
    versions(root, &manifest)?;
    require(
        manifest["skills"] == "./skills/",
        "Некорректный каталог навыков",
    )?;
    let extension = &manifest["extensions"]["com.openai"];
    if truthy(&extension["onboardingSkill"]) {
        contained(root, root, string(extension, "onboardingSkill")?, false)?;
    }
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
    let listing = read(&root.join("docs/skills.md"))?;
    check_catalog(&listing, &skills)?;
    for name in &skills {
        let folder = root.join("skills").join(name);
        for path in entries(&folder)? {
            if skill_markdown(&path) {
                metadata(&path)?;
                skill_structure(&path)?;
            }
        }
        require(
            folder.join("SKILL.md").is_file(),
            format!("{name}: отсутствует SKILL.md"),
        )?;
        resources(&folder, root)?;
        skill_ui(root, &folder, name)?;
        for path in entries(&folder.join("agents"))? {
            if path
                .file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with("openai"))
                && extension_matches(&path, "yaml")
            {
                let ui: Value = serde_yaml::from_str(&read(&path)?).map_err(|e| e.to_string())?;
                require(
                    ui["policy"]["allow_implicit_invocation"] == false,
                    format!("{name}: навыки fstack требуют явного включения"),
                )?;
            }
        }
    }
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
    for asset in assets {
        let path = contained(root, root, asset, false)?;
        require(
            fs::read(&path)
                .map_err(|e| e.to_string())?
                .starts_with(b"\x89PNG\r\n\x1a\n"),
            "Ожидался PNG",
        )?;
    }
    let role_paths: Vec<_> = entries(&root.join("profile/agents"))?
        .into_iter()
        .filter(|p| extension_matches(p, "toml"))
        .collect();
    let expected: HashSet<_> = ROLES.iter().copied().collect();
    let actual: HashSet<_> = role_paths
        .iter()
        .filter_map(|p| p.file_stem().and_then(|n| n.to_str()))
        .collect();
    require(
        actual == expected,
        "Набор ролей не совпадает с синхронизацией",
    )?;
    for path in &role_paths {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .ok_or("Некорректное имя роли")?;
        let role = toml_file(path)?;
        require(
            role["name"] == name
                && nonempty(&role["description"])
                && nonempty(&role["developer_instructions"]),
            format!("Некорректная роль: {name}"),
        )?;
        role_settings(&role, name)?;
        references(
            string(&role, "developer_instructions")?,
            &skills,
            rx!(r"fstack:([a-z0-9-]+)"),
            false,
        )?;
    }
    let config = toml_file(&root.join("profile/config.example.toml"))?;
    let agents = &config["agents"];
    require(
        agents["enabled"].is_boolean(),
        "agents.enabled должен быть boolean",
    )?;
    if let Some(limit) = agents.get("max_concurrent_threads_per_session") {
        require(
            limit.as_u64().is_some_and(|v| v > 0),
            "Лимит подагентов должен быть положительным целым числом",
        )?;
    }
    for (settings, model, effort) in [
        (&config, "model", "model_reasoning_effort"),
        (
            agents,
            "default_subagent_model",
            "default_subagent_reasoning_effort",
        ),
    ] {
        require(
            nonempty(&settings[model]),
            format!("Некорректная модель примера: {model}"),
        )?;
        require(
            EFFORTS.contains(&string(settings, effort)?),
            format!("Некорректный reasoning: {effort}"),
        )?;
    }
    contained(root, root, "tools/fstack/src/profile.rs", false)?;
    for path in descendants(root, &[".git", ".venv", "work"])? {
        if extension_matches(&path, "md") {
            let text = read(&path)?;
            references(&text, &skills, rx!(r"\bfstack:([a-z][a-z0-9-]*)"), true)?;
            links(&path, root, &text)?;
        }
    }
    Ok(format!(
        "Пакет {}: {} навыков, метаданные, ссылки и ресурсы проверены.",
        string(&manifest, "version")?,
        skills.len()
    ))
}

fn main_result() -> Result<()> {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let package = package_root(&mut arguments)?;
    let command = arguments.first().and_then(|s| s.to_str()).unwrap_or("");
    match (command, arguments.len()) {
        ("memory", _) => return memory::run(&arguments[1..], &package),
        ("profile", _) => {
            if let Err(error) = profile::run(&arguments[1..], &package) {
                let value: Value = serde_json::from_str(&error).unwrap_or_else(|_| json!({"status":"error", "message": error}));
                println!("{value}");
                std::process::exit(if value["status"] == "conflict" { 1 } else { 2 });
            }
            return Ok(());
        },
        ("release", _) => return release::run(&arguments[1..], &package),
        ("check-installation", _) => return installation::run(&arguments[1..], &package),
        ("check-package", 2) => println!("{}", check(Path::new(&arguments[1]))?),
        ("resources", 3) => resources(Path::new(&arguments[2]), Path::new(&arguments[1]))?,
        ("metadata", 2) => println!("{}", metadata(Path::new(&arguments[1]))?),
        ("structure", 2) => skill_structure(Path::new(&arguments[1]))?,
        ("role-settings", 2) => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())?;
            let role: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
            role_settings(&role, arguments[1].to_str().ok_or("Некорректное имя роли")?)?;
        }
        _ => return Err("Использование: fstack [--package ROOT] memory | profile | release | check-installation | check-package ROOT | resources ROOT FOLDER | metadata PATH | structure PATH | role-settings NAME (JSON stdin)".into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = main_result() {
        eprintln!("Проверка не пройдена: {error}");
        std::process::exit(1);
    }
}

fn package_root(arguments: &mut Vec<std::ffi::OsString>) -> Result<PathBuf> {
    if arguments.first().is_some_and(|a| a == "--package") {
        require(
            arguments.len() >= 3,
            "После --package нужен каталог и команда",
        )?;
        let root = PathBuf::from(arguments.remove(1));
        arguments.remove(0);
        return root.canonicalize().map_err(|e| e.to_string());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    executable
        .ancestors()
        .skip(1)
        .find(|p| p.join(".codex-plugin/plugin.json").is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| "Не найден пакет рядом с исполняемым файлом; укажите --package ROOT".into())
}

mod installation;
mod memory;
#[cfg(test)]
mod package_tests;
mod profile;
mod release;

fn native_notices(root: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let inventory = json_file(&root.join("licenses/native-runtime.json"))?;
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
            && files.contains_key("licenses/native-runtime.txt")
            && files.contains_key("licenses/native-runtime-rust-1.90.0.html"),
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
