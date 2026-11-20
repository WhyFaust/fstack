use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    folder: PathBuf,
    skill: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        let folder = root.join("skills/example");
        fs::create_dir_all(&folder).unwrap();
        let skill = folder.join("SKILL.md");
        Self {
            _temp: temp,
            root,
            folder,
            skill,
        }
    }
    fn metadata() -> Value {
        json!({"name":"example","description":"Применяй для примера.","license":"MIT","metadata":{"author":"WhyFaust","source":"https://github.com/WhyFaust/fstack","version":"2.0.0"}})
    }
    fn write(&self, value: &Value) {
        fs::write(
            &self.skill,
            format!(
                "---\n{}---\n# Пример\n",
                serde_yaml::to_string(value).unwrap()
            ),
        )
        .unwrap();
    }
}

#[test]
fn frontmatter_validates_types_and_name() {
    let f = Fixture::new();
    let base = Fixture::metadata();
    f.write(&base);
    assert_eq!(metadata(&f.skill).unwrap(), base);
    for field in ["name", "description"] {
        let mut value = base.clone();
        value.as_object_mut().unwrap().remove(field);
        f.write(&value);
        assert!(metadata(&f.skill).unwrap_err().contains(field));
    }
    for changes in [
        json!({"name":"example--invalid"}),
        json!({"name":"another"}),
        json!({"description":""}),
        json!({"description":123}),
        json!({"description":"x".repeat(1025)}),
        json!({"license":null}),
        json!({"compatibility":["Rust"]}),
        json!({"compatibility":"x".repeat(501)}),
        json!({"allowed-tools":[]}),
        json!({"metadata":{"author":"WhyFaust","version":2}}),
        json!({"disable-model-invocation":"true"}),
    ] {
        let mut value = base.clone();
        for (key, v) in changes.as_object().unwrap() {
            value[key] = v.clone();
        }
        f.write(&value);
        assert!(metadata(&f.skill).is_err(), "{changes}");
    }
    f.write(&base);
    let text = read(&f.skill).unwrap();
    for key in ["123", "true"] {
        fs::write(
            &f.skill,
            text.replace("metadata:\n", &format!("metadata:\n  {key}: text\n")),
        )
        .unwrap();
        assert!(metadata(&f.skill).is_err());
    }
}

#[test]
fn frontmatter_requires_delimiter_lines() {
    let f = Fixture::new();
    let header = "---\r\nname: example\r\ndescription: Текст --- примера.\r\n";
    fs::write(&f.skill, format!("{header}---\r\nТекст\r\n")).unwrap();
    assert_eq!(
        metadata(&f.skill).unwrap()["description"],
        "Текст --- примера."
    );
    fs::write(&f.skill, format!("{header}--- текст")).unwrap();
    assert!(metadata(&f.skill)
        .unwrap_err()
        .contains("Отсутствует frontmatter"));
}

#[test]
fn skill_ui_validates_types_limits_policy_and_dependencies() {
    let f = Fixture::new();
    let path = f.folder.join("openai.yaml");
    let base = json!({"interface":{
        "display_name":"Пример",
        "short_description":"Проверка полезного результата навыка",
        "default_prompt":"Используй $fstack:example для задачи."
    }});
    let validate = |data: &Value| {
        fs::write(&path, serde_yaml::to_string(data).unwrap()).unwrap();
        skill_ui_file(&f.root, &f.folder, "example", &path)
    };
    validate(&base).unwrap();
    validate(&json!({"interface":{"display_name":"Пример"}})).unwrap();
    for policy in [
        json!({}),
        json!({"allow_implicit_invocation":true}),
        json!({"allow_implicit_invocation":false}),
    ] {
        let mut data = base.clone();
        data["policy"] = policy;
        validate(&data).unwrap();
    }
    let mut bounds = base.clone();
    bounds["interface"]["display_name"] = json!(format!("  {} \n", "Я".repeat(64)));
    bounds["interface"]["short_description"] = json!("Я".repeat(64));
    bounds["interface"]["default_prompt"] = json!(format!("$fstack:example {}", "Я".repeat(1008)));
    validate(&bounds).unwrap();
    for (section, field, value) in [
        ("interface", "display_name", json!(123)),
        ("interface", "display_name", json!("Я".repeat(65))),
        ("interface", "short_description", json!("Я".repeat(24))),
        ("interface", "short_description", json!("Я".repeat(65))),
        ("interface", "default_prompt", json!("Use $example")),
        (
            "interface",
            "default_prompt",
            json!("Use $fstack:example-sibling"),
        ),
        (
            "interface",
            "default_prompt",
            json!(format!("$fstack:example {}", "Я".repeat(1009))),
        ),
        ("interface", "brand_color", json!(123)),
        ("interface", "brand_color", json!("blue")),
        ("policy", "allow_implicit_invocation", json!("false")),
        ("dependencies", "tools", json!({})),
    ] {
        let mut data = base.clone();
        data[section][field] = value;
        assert!(validate(&data).is_err(), "{section}.{field}");
    }
    for (field, value) in [
        ("interface", json!([])),
        ("policy", json!(false)),
        ("dependencies", json!([])),
    ] {
        let mut data = base.clone();
        data[field] = value;
        assert!(validate(&data).is_err(), "{field}");
    }
    for dependency in [
        json!({"type":"mcp","value":"existing"}),
        json!({"type":"mcp","value":"docs","url":"https://example.invalid/mcp"}),
        json!({"type":"mcp","value":"local","transport":"stdio","command":"local-mcp"}),
        json!({"type":"mcp","value":"custom","transport":"sse"}),
    ] {
        let mut data = base.clone();
        data["dependencies"]["tools"] = json!([dependency]);
        validate(&data).unwrap();
        for (field, value) in [
            ("type", json!("cli")),
            ("value", json!(null)),
            ("description", json!(123)),
            ("transport", json!(123)),
            ("url", json!("Я".repeat(1025))),
            ("command", json!(false)),
        ] {
            let mut invalid = data.clone();
            invalid["dependencies"]["tools"][0][field] = value;
            assert!(validate(&invalid).is_err(), "{field}");
        }
    }
}

#[test]
fn skill_ui_icons_resolve_only_packaged_assets() {
    let f = Fixture::new();
    fs::create_dir(f.folder.join("assets")).unwrap();
    fs::create_dir(f.root.join("assets")).unwrap();
    fs::write(f.folder.join("assets/local.svg"), "local").unwrap();
    fs::write(f.root.join("assets/shared.svg"), "shared").unwrap();
    fs::write(f.folder.join("outside.svg"), "outside").unwrap();
    let alias = f.root.join("aliases");
    link_dir(&f.root.join("assets"), &alias);
    let path = f.folder.join("openai.yaml");
    let mut data = json!({"interface":{
        "display_name":"Пример",
        "short_description":"Проверка полезного результата навыка",
        "default_prompt":"Используй $fstack:example для задачи.",
        "brand_color":"#3b82F6"
    }});
    for (icon, valid) in [
        ("./assets/local.svg".to_string(), true),
        ("../../assets/shared.svg".to_string(), true),
        ("../../aliases/shared.svg".to_string(), false),
        ("assets/missing.svg".to_string(), false),
        ("outside.svg".to_string(), false),
        ("assets/../outside.svg".to_string(), false),
        (
            f.root
                .join("assets/shared.svg")
                .to_string_lossy()
                .into_owned(),
            false,
        ),
    ] {
        data["interface"]["icon_small"] = json!(icon);
        fs::write(&path, serde_yaml::to_string(&data).unwrap()).unwrap();
        assert_eq!(
            skill_ui_file(&f.root, &f.folder, "example", &path).is_ok(),
            valid,
            "{icon}"
        );
    }
    #[cfg(windows)]
    fs::remove_dir(alias).unwrap();
}

#[test]
fn resources_allow_shared_files_and_reject_missing_escape() {
    let f = Fixture::new();
    fs::write(f.root.join("LICENSE"), "notice").unwrap();
    fs::write(&f.skill, "[License](../../LICENSE)\n").unwrap();
    resources(&f.folder, &f.root).unwrap();
    fs::write(f.root.parent().unwrap().join("outside.md"), "external").unwrap();
    for target in ["references/missing.md", "../../../outside.md"] {
        fs::write(&f.skill, format!("[Resource]({target})")).unwrap();
        assert!(resources(&f.folder, &f.root).is_err());
    }
    fs::write(f.folder.join("space name.md"), "## Section\n").unwrap();
    fs::write(&f.skill,"[Present](space%20name.md#section)\n`[Example](missing.md)`\n~~~~md\n[Example](missing.md)\n~~~~\n").unwrap();
    resources(&f.folder, &f.root).unwrap();
    fs::write(&f.skill, "[Missing](space%20missing.md)").unwrap();
    assert!(resources(&f.folder, &f.root).is_err());
}

#[test]
fn packaged_markdown_links_resolve_rendered_sections() {
    let f = Fixture::new();
    let guide = f.folder.join("Краткий обзор.md");
    fs::write(
        &guide,
        concat!(
            "## Шаг `A_B`: [API](#шаг-a_b-api)\n",
            "Шаг `A_B`: API\n------------\n",
            "## Шаг A_B API-1\n",
            "## Шаг A_B API\n",
            "## 東京 — café!\n",
            "<a name=\"manual-id\"></a>\n",
            "<span id=\"A&amp;B\"></span>\n",
            "<!-- <a name=\"hidden\"></a> -->\n",
            "\n```md\n## Только пример\n```\n",
        ),
    )
    .unwrap();
    let dest = "Краткий%20обзор.md";
    fs::write(
        &f.skill,
        format!(
            "# Местный заголовок\n[Здесь](#местный-заголовок)\n\n\
         [Первый]({dest}#шаг-a_b-api) [Повтор]({dest}#шаг-a_b-api-1)\n\
         [Коллизия]({dest}#шаг-a_b-api-1-1) [Следующий]({dest}#шаг-a_b-api-2)\n\
         [Unicode][section] [HTML]({dest}#manual-id)\n\n\
         [section]: <Краткий обзор.md#東京--caf%C3%A9> \"Раздел\"\n\n\
         <a href=\"{dest}#a%26b\">Раздел HTML</a>\n\n\
         `[Образец](#нет)`\n\n~~~~\n[Образец](нет.md#нет)\n~~~~\n"
        ),
    )
    .unwrap();
    resources(&f.folder, &f.root).unwrap();
    for target in ["#несуществующий", "Краткий%20обзор.md#только-пример"]
    {
        fs::write(&f.skill, format!("[Раздел][id]\n\n[id]: {target}")).unwrap();
        let error = resources(&f.folder, &f.root).unwrap_err();
        assert!(
            error.contains("SKILL.md") && error.contains(target),
            "{error}"
        );
    }
    fs::write(&f.skill, format!("<a href=\"{dest}#hidden\">Проверка</a>")).unwrap();
    assert!(resources(&f.folder, &f.root)
        .unwrap_err()
        .contains("#hidden"));
}

#[test]
fn optional_resource_directory_requires_content() {
    let f = Fixture::new();
    fs::write(&f.skill, "").unwrap();
    let dir = f.folder.join("references");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join(".gitkeep"), "").unwrap();
    assert!(resources(&f.folder, &f.root).is_err());
    fs::write(dir.join("contract.md"), "contract").unwrap();
    resources(&f.folder, &f.root).unwrap();
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
fn link_dir(target: &Path, link: &Path) {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let cmd_path = |path: &Path| {
        std::ffi::OsString::from_wide(
            &path
                .as_os_str()
                .encode_wide()
                .map(|unit| {
                    if unit == b'/' as u16 {
                        b'\\' as u16
                    } else {
                        unit
                    }
                })
                .collect::<Vec<_>>(),
        )
    };
    let output = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(cmd_path(link))
        .arg(cmd_path(target))
        .output()
        .expect("run mklink for resource-containment fixture");
    assert!(
        output.status.success(),
        "mklink /J {} {} failed: {}\n{}",
        link.display(),
        target.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
#[test]
fn nested_resource_directory_links_are_checked_after_valid_files() {
    let f = Fixture::new();
    fs::write(&f.skill, "").unwrap();
    let scripts = f.folder.join("scripts");
    fs::create_dir(&scripts).unwrap();
    fs::write(scripts.join("local.rs"), "fn main() {}").unwrap();
    let outside = f.root.parent().unwrap().join("external");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("helper.rs"), "fn main() {}").unwrap();
    let link = scripts.join("vendor");
    link_dir(&outside, &link);
    assert!(resources(&f.folder, &f.root).is_err());
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    let shared = f.root.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("helper.rs"), "fn main() {}").unwrap();
    link_dir(&shared, &link);
    resources(&f.folder, &f.root).unwrap();
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for path in entries(from).unwrap() {
        if [".git", "target", "__pycache__"]
            .iter()
            .any(|n| path.file_name().unwrap() == *n)
        {
            continue;
        }
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &dest)
        } else {
            fs::copy(path, dest).unwrap();
        }
    }
}

#[test]
fn complete_package_accepts_minimal_skills_with_explicit_invocation() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    let folder = package.join("skills/no-comments");
    let skill = folder.join("SKILL.md");
    let ui = folder.join("agents/openai.yaml");
    let header = "---\nname: no-comments\ndescription: Проверяет код.\n";
    let body = "---\n\nПроверь код.\n";
    let policy = "policy:\n  allow_implicit_invocation: false\n";

    fs::write(
        &skill,
        format!("{header}disable-model-invocation: true\n{body}"),
    )
    .unwrap();
    fs::write(&ui, policy).unwrap();
    check(&package).unwrap();

    for empty in ["", "\r\n \t\r\n"] {
        fs::write(
            &skill,
            format!("{header}disable-model-invocation: true\n---{empty}"),
        )
        .unwrap();
        assert!(check(&package)
            .unwrap_err()
            .contains("отсутствует текст инструкции"));
    }

    for value in ["false", "\"true\""] {
        fs::write(
            &skill,
            format!("{header}disable-model-invocation: {value}\n{body}"),
        )
        .unwrap();
        assert!(check(&package)
            .unwrap_err()
            .contains("disable-model-invocation"));
    }

    fs::write(&skill, format!("{header}{body}")).unwrap();
    assert!(check(&package)
        .unwrap_err()
        .contains("disable-model-invocation"));
    fs::write(
        &skill,
        format!("{header}disable-model-invocation: true\n{body}"),
    )
    .unwrap();
    for value in ["true", "\"false\"", "null"] {
        fs::write(&ui, policy.replace("false", value)).unwrap();
        assert!(check(&package).unwrap_err().contains("openai.yaml"));
    }
    fs::remove_file(&ui).unwrap();
    assert!(check(&package).unwrap_err().contains("openai.yaml"));
}

#[test]
fn complete_package_rejects_broken_release_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    check(&package).unwrap();
    let translation = package.join("skills/write/SKILL_en.md");
    let original = read(&translation).unwrap();
    let data = metadata(&translation).unwrap();
    let version = data["metadata"]["version"].as_str().unwrap();
    for replacement in ["0.0.0", ""] {
        fs::write(&translation, original.replacen(version, replacement, 1)).unwrap();
        assert!(check(&package).unwrap_err().contains("metadata.version"));
    }
    fs::write(translation, original).unwrap();
    for (relative, key, value) in [
        (".codex-plugin/plugin.json", "version", json!("01.0.0")),
        (".codex-plugin/plugin.json", "skills", json!("./missing/")),
        (".agents/plugins/marketplace.json", "plugins", json!([])),
        ("hooks/hooks.json", "hooks", json!({})),
    ] {
        let path = package.join(relative);
        let original = fs::read(&path).unwrap();
        let mut data = json_file(&path).unwrap();
        data[key] = value;
        fs::write(&path, data.to_string()).unwrap();
        assert!(check(&package).is_err(), "{relative}:{key}");
        fs::write(&path, original).unwrap();
    }
    let hooks = json_file(&package.join("hooks/hooks.json")).unwrap();
    let mut synchronous = hooks["hooks"]["Stop"][0]["hooks"][0].clone();
    synchronous["async"] = json!("false");
    assert!(check_hook_command("Stop", &synchronous).is_err());
    for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
        let mut handler = hooks["hooks"][event][0]["hooks"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        handler
            .as_object_mut()
            .unwrap()
            .remove("additionalContextLimit");
        assert!(check_hook_command(event, &handler)
            .unwrap_err()
            .contains("additionalContextLimit"));
    }
    let stamp = package.join("launchers/package-version");
    let original = fs::read(&stamp).unwrap();
    fs::write(&stamp, "0.0.0\n").unwrap();
    assert!(check(&package).unwrap_err().contains("Версия запускателя"));
    fs::write(stamp, original).unwrap();
    let localized = package.join("skills/skill/agents/openai_en.yaml");
    fs::write(&localized, "interface:\n  display_name: 123\n").unwrap();
    assert!(check(&package).is_err());
    fs::remove_file(localized).unwrap();
    let canonical = package.join("skills/skill/agents/openai.yaml");
    let original = fs::read(&canonical).unwrap();
    let upper = canonical.with_file_name("OPENAI.yaml");
    let interim = canonical.with_file_name("renaming.tmp");
    fs::rename(&canonical, &interim).unwrap();
    fs::rename(&interim, &upper).unwrap();
    if canonical.is_file() {
        check(&package).unwrap();
        fs::write(&upper, "interface:\n  display_name: 123\n").unwrap();
    }
    assert!(check(&package).is_err());
    fs::rename(&upper, &interim).unwrap();
    fs::rename(&interim, &canonical).unwrap();
    fs::write(canonical, original).unwrap();
    let icon = package.join("assets/fstack.png");
    fs::write(icon, b"not PNG").unwrap();
    assert!(check(&package).is_err());
}
#[test]
fn package_rejects_a_bundled_global_role_or_onboarding_skill() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    let role = package.join("profile/agents/default.toml");
    fs::create_dir_all(role.parent().unwrap()).unwrap();
    fs::write(&role, r#"name = "default""#).unwrap();
    assert!(check(&package).unwrap_err().contains("profile/agents"));
    fs::remove_dir_all(package.join("profile")).unwrap();

    let path = package.join(".codex-plugin/plugin.json");
    let mut manifest = json_file(&path).unwrap();
    manifest["extensions"] =
        json!({"com.openai":{"onboardingSkill":"./skills/faust-mode/SKILL.md"}});
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(check(&package).unwrap_err().contains("onboardingSkill"));
}
#[test]
fn package_rejects_instructions_for_a_removed_skill() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    let guide = package.join("docs").join("obsolete.md");
    fs::write(&guide, "Use $fstack:profile").unwrap();
    let error = check(&package).unwrap_err();
    assert!(error.contains("obsolete.md") && error.contains("отсутствует навык profile"));
}

#[test]
fn check_package_follows_links_in_documentation_and_skill_resources() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    for path in [
        "work/notes.md",
        "tools/fstack/target/private.md",
        ".codex/local.md",
    ] {
        let path = package.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[Черновик](#нет)").unwrap();
    }
    check(&package).unwrap();
    for relative in [
        "README.md",
        "AGENTS.md",
        "docs/examples.md",
        "skills/delegate/worker.md",
    ] {
        let path = package.join(relative);
        let original = read(&path).unwrap();
        fs::write(
            &path,
            format!("{original}\n\n[Правило](/docs/skills.md#исчезнувший-раздел)\n"),
        )
        .unwrap();
        let error = check(&package).unwrap_err();
        assert!(
            error.contains(path.file_name().unwrap().to_str().unwrap())
                && error.contains("#исчезнувший-раздел"),
            "{error}"
        );
        fs::write(&path, original).unwrap();
    }
}
#[cfg(windows)]
#[test]
fn case_variants_cannot_skip_validation() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    let skill = package.join("skills/no-comments/SKILL.md");
    let original = read(&skill).unwrap();
    let interim = skill.with_file_name("renaming.tmp");
    fs::rename(&skill, &interim).unwrap();
    let lower = skill.with_file_name("skill.md");
    fs::rename(interim, &lower).unwrap();
    fs::write(&lower, original.replace("name: no-comments", "name: 123")).unwrap();
    assert!(check(&package).is_err());
    fs::write(&lower, original).unwrap();
    check(&package).unwrap();
    fs::write(package.join("UPPER.MD"), "[Missing](missing.md)").unwrap();
    assert!(check(&package).is_err());
}

#[test]
fn native_notices_track_locked_dependencies_and_shipped_texts() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    native_notices(&package).unwrap();
    for relative in [
        "tools/fstack/Cargo.lock",
        "tools/fstack/Cargo.toml",
        "tools/fstack/native-runtime.txt",
        "tools/fstack/native-runtime-rust-1.90.0.html",
    ] {
        let path = package.join(relative);
        let original = fs::read(&path).unwrap();
        let mut modified = original.clone();
        modified.push(b'\n');
        fs::write(&path, modified).unwrap();
        assert!(native_notices(&package).is_err(), "{relative}");
        fs::write(&path, original).unwrap();
    }
    let path = package.join("tools/fstack/native-runtime.json");
    let original = json_file(&path).unwrap();
    for field in ["packages", "excluded_packages"] {
        let mut inventory = original.clone();
        inventory[field].as_array_mut().unwrap().remove(0);
        fs::write(&path, inventory.to_string()).unwrap();
        assert!(native_notices(&package).is_err(), "{field}");
    }
    let mut inventory = original;
    inventory["packages"][0]["checksum"] = json!("wrong");
    fs::write(path, inventory.to_string()).unwrap();
    assert!(native_notices(&package).is_err());
}

#[test]
fn native_notices_accept_the_legacy_layout_until_a_current_inventory_is_present() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path();
    let source = root().join("tools/fstack");
    let current = package.join("tools/fstack");
    let legacy = package.join("licenses");
    fs::create_dir_all(&current).unwrap();
    fs::create_dir(&legacy).unwrap();
    for name in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(source.join(name), current.join(name)).unwrap();
    }
    for name in ["native-runtime.txt", "native-runtime-rust-1.90.0.html"] {
        fs::copy(source.join(name), legacy.join(name)).unwrap();
    }
    let inventory = read(&source.join("native-runtime.json")).unwrap();
    let old_inventory = inventory.replace("tools/fstack/native-runtime", "licenses/native-runtime");
    fs::write(legacy.join("native-runtime.json"), &old_inventory).unwrap();
    native_notices(package).unwrap();

    fs::write(current.join("native-runtime.json"), old_inventory).unwrap();
    assert!(native_notices(package)
        .unwrap_err()
        .contains("Неполный комплект уведомлений"));
}

#[test]
fn catalog_links_must_open_the_named_skill() {
    let skills = BTreeSet::from(["read".to_owned(), "write".to_owned()]);
    let listing = "| `read` | Чтение | [RU](../skills/read/SKILL.md), [EN](../skills/read/SKILL_en.md) |\n| `write` | Запись | [RU](../skills/write/SKILL.md) |";
    assert!(check_catalog(listing, &skills).is_ok());
    assert!(check_catalog(&listing.replace("read/SKILL.md", "write/SKILL.md"), &skills).is_err());
    assert!(check_catalog(&format!("{listing}\n{listing}"), &skills).is_err());
    let linked = "| [read](../skills/read/SKILL.md) | Чтение |\n| [write](../skills/write/SKILL.md) | Запись |";
    assert!(check_catalog(linked, &skills).is_ok());
    assert!(check_catalog(&linked.replace("read/SKILL.md", "write/SKILL.md"), &skills).is_err());
    assert!(check_catalog(&format!("{linked}\n{linked}"), &skills).is_err());
}
