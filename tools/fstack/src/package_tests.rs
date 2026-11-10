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
        json!({"metadata":{"author":"another","source":"https://github.com/WhyFaust/fstack"}}),
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
    fs::write(f.folder.join("space name.md"), "present").unwrap();
    fs::write(&f.skill,"[Present](space%20name.md#section)\n`[Example](missing.md)`\n~~~~md\n[Example](missing.md)\n~~~~\n").unwrap();
    resources(&f.folder, &f.root).unwrap();
    fs::write(&f.skill, "[Missing](space%20missing.md)").unwrap();
    assert!(resources(&f.folder, &f.root).is_err());
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

#[test]
fn structure_ignores_fences_and_requires_order_and_content() {
    let f = Fixture::new();
    f.write(&Fixture::metadata());
    let header = read(&f.skill).unwrap();
    let sections="## Вход и границы\nВход.\n## Порядок работы\n```md\n## Чужой заголовок\n```\n## Проверка и результат\nРезультат.\n";
    fs::write(&f.skill, format!("{header}{sections}")).unwrap();
    skill_structure(&f.skill).unwrap();
    for body in [
        sections.replace("## Вход и границы", "## Иной раздел"),
        sections.replace("Результат.", ""),
        format!("{sections}# Второй заголовок\n"),
        sections.replace("## Порядок работы", "## Проверка и результат"),
    ] {
        fs::write(&f.skill, format!("{header}{body}")).unwrap();
        assert!(skill_structure(&f.skill).is_err());
    }
    let english = f.folder.join("SKILL_en.md");
    fs::write(&english,format!("{header}## Inputs and scope\nInput.\n## Procedure\nDo it.\n## Verification and result\nCheck.\n")).unwrap();
    skill_structure(&english).unwrap();
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
fn complete_package_rejects_broken_release_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    copy_tree(&root(), &package);
    check(&package).unwrap();
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
    let icon = package.join("assets/fstack.png");
    fs::write(icon, b"not PNG").unwrap();
    assert!(check(&package).is_err());
}
#[test]
fn shipped_role_settings_validate_model_and_effort() {
    for path in entries(&root().join("profile/agents")).unwrap() {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let role = toml_file(&path).unwrap();
        role_settings(&role, name).unwrap();
        let mut custom = role.clone();
        custom["model"] = json!("custom-provider/model");
        role_settings(&custom, name).unwrap();
        for (field, value) in [("model", ""), ("model_reasoning_effort", "unknown")] {
            let mut invalid = role.clone();
            invalid[field] = json!(value);
            assert!(role_settings(&invalid, name).is_err());
        }
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
    fs::write(&lower, original.replace("license: MIT", "license: 123")).unwrap();
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
        "licenses/native-runtime.txt",
        "licenses/native-runtime-rust-1.90.0.html",
    ] {
        let path = package.join(relative);
        let original = fs::read(&path).unwrap();
        let mut modified = original.clone();
        modified.push(b'\n');
        fs::write(&path, modified).unwrap();
        assert!(native_notices(&package).is_err(), "{relative}");
        fs::write(&path, original).unwrap();
    }
    let path = package.join("licenses/native-runtime.json");
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
