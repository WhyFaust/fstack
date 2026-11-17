use super::*;
use std::cell::{Cell, RefCell};

#[test]
fn migration_notes_keep_code_and_skip_examples_and_template_hints() {
    let example = "~~~md\n## При обновлении\nПример\n~~~\n\n> ## При обновлении\n";
    assert_eq!(upgrade_section(example), None);
    let steps = "Сохраните настройки.\n\n```sh\n# Комментарий\ncat settings\n<!-- literal -->\n```";
    let body = format!("{example}\n## При обновлении\n\n<!-- удалите\nподсказку -->\n{steps}\n\n  ## Проверка\nГотово.");
    assert_eq!(upgrade_section(&body).as_deref(), Some(steps));
    assert_eq!(upgrade_section("## При обновлении\n<!-- пусто -->"), None);
    for heading in ["##  При обновлении", "При обновлении\n--------------"]
    {
        let body = format!("{heading}\n\n    команда\n");
        assert_eq!(upgrade_section(&body).as_deref(), Some("    команда"));
    }
}

struct Stub {
    records: RefCell<Vec<Value>>,
    exact: RefCell<Value>,
    version_pull: RefCell<Option<(String, Value)>>,
    created_notes: RefCell<String>,
    calls: RefCell<Vec<Vec<String>>>,
    publish_tag: bool,
    fail_release_action: Cell<Option<&'static str>>,
}
impl Runner for Stub {
    fn check(&self, root: &Path) -> Result<()> {
        check_versions(root).map(|_| ())
    }
    fn run(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        if args[0] != "gh" {
            return System.run(root, args, input);
        }
        if args[1] == "api" {
            let response = if args[2] == "graphql" {
                json!({"data":{"repository":{"release":*self.exact.borrow()}}})
            } else if args.last().unwrap().ends_with("/pulls?per_page=100") {
                let pulls = self.version_pull.borrow();
                match pulls.as_ref() {
                    Some((commit, pull))
                        if args
                            .last()
                            .unwrap()
                            .contains(&format!("/commits/{commit}/")) =>
                    {
                        json!([[pull]])
                    }
                    _ => json!([[]]),
                }
            } else {
                json!([*self.records.borrow()])
            };
            return serde_json::to_vec(&response).map_err(|e| e.to_string());
        }
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        if args[1] == "release" && self.fail_release_action.get() == Some(args[2]) {
            return Err(format!("Сбой gh release {}", args[2]));
        }
        if args[1..3] == ["release", "create"] {
            assert!(args.windows(2).any(|pair| pair == ["--notes-file", "-"]));
            *self.created_notes.borrow_mut() = String::from_utf8(input.unwrap().to_vec()).unwrap();
        }
        if self.publish_tag && args[1..3] == ["release", "edit"] {
            let target = args[args.iter().position(|s| *s == "--target").unwrap() + 1];
            assert!(args.contains(&"--prerelease=false"));
            System.run(root, &["git", "tag", args[3], target], None)?;
            System.run(root, &["git", "push", "origin", args[3]], None)?;
            for record in self.records.borrow_mut().iter_mut() {
                if record["tag_name"] == args[3] {
                    record["target_commitish"] = json!(target);
                    record["prerelease"] = json!(false);
                    record["draft"] = json!(false);
                }
            }
        }
        Ok(Vec::new())
    }
}
fn stub() -> Stub {
    Stub {
        exact: RefCell::new(Value::Null),
        version_pull: RefCell::new(None),
        created_notes: RefCell::new(String::new()),
        records: RefCell::new(vec![
            json!({"tag_name":"v0.2.0","draft":false,"prerelease":false}),
        ]),
        calls: RefCell::new(vec![]),
        publish_tag: false,
        fail_release_action: Cell::new(None),
    }
}
fn git(root: &Path, args: &[&str]) -> String {
    let mut command = vec!["git", "-c", "commit.gpgsign=false"];
    command.extend(args);
    String::from_utf8(System.run(root, &command, None).unwrap())
        .unwrap()
        .trim()
        .into()
}
fn save(root: &Path, name: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn commit(root: &Path, message: &str) {
    git(root, &["add", "."]);
    git(root, &["commit", "-m", message]);
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    save(
        &root,
        MANIFEST,
        json!({"name":"fstack","version":"0.2.0","interface":{
            "composerIcon":"./media/icon.png","logo":"./assets/logo.png","screenshots":["./gallery/shot.png"]
        }}).to_string(),
    );
    save(&root, PACKAGE_VERSION, "0.2.0\n");
    save(&root, "launchers/package-major", "0\n");
    save(&root, ".gitignore", b"dist/\n");
    for name in ["alpha", "beta"] {
        save(
            &root,
            &format!("skills/{name}/SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Fixture skill\ndisable-model-invocation: true\n---\n# Original body {name}\n"
            ),
        );
    }
    git(&root, &["init", "--initial-branch=main"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    commit(&root, "chore: baseline");
    let release = Release {
        root: &root,
        runner: &System,
        artifacts: None,
    };
    save(
        &root,
        RUNTIME_ID,
        format!("{}\n", release.runtime_id("HEAD").unwrap()),
    );
    git(&root, &["add", RUNTIME_ID]);
    git(&root, &["commit", "--amend", "--no-edit"]);
    git(&root, &["tag", "v0.2.0"]);
    (temp, root)
}
fn origin(root: &Path) {
    let bare = root.parent().unwrap().join("origin.git");
    git(
        root.parent().unwrap(),
        &[
            "clone",
            "--bare",
            &root.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    git(root, &["remote", "add", "origin", &bare.to_string_lossy()]);
}
fn remote_marketplace(root: &Path) -> String {
    git(
        root,
        &["ls-remote", "--heads", "origin", "refs/heads/marketplace/*"],
    )
}
fn artifacts(root: &Path, sha: &str) -> PathBuf {
    let directory = root.parent().unwrap().join("artifacts");
    for target in TARGETS {
        let input = root.parent().unwrap().join("fixture-binary");
        fs::write(&input, format!("fixture binary for {target}")).unwrap();
        artifact(
            &[
                "--target".into(),
                target.into(),
                "--binary".into(),
                input.into_os_string(),
                "--output".into(),
                directory.as_os_str().to_owned(),
            ],
            root,
        )
        .unwrap();
        let descriptor: Value = serde_json::from_slice(
            &fs::read(directory.join(target).join("artifact.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(descriptor["source_sha"], sha);
    }
    directory
}
fn event(sha: &str) -> BTreeMap<String, String> {
    [
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_REPOSITORY", "fixture/fstack"),
        ("GITHUB_SHA", sha),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}
#[test]
fn event_and_live_main_reject_unvalidated_commits() {
    let (_temp, root) = fixture();
    let base = git(&root, &["rev-parse", "HEAD"]);
    save(&root, "other.txt", "New push");
    commit(&root, "fix: unvalidated");
    let newer = git(&root, &["rev-parse", "HEAD"]);
    origin(&root);
    git(&root, &["checkout", "--detach", &base]);
    let artifacts = artifacts(&root, &base);
    let stub = stub();
    let release = Release {
        root: &root,
        runner: &stub,
        artifacts: Some(&artifacts),
    };
    for (sha, reason) in [(&newer, "проверенного SHA"), (&base, "main изменился")]
    {
        assert!(release
            .publish("fixture/fstack", &event(sha))
            .unwrap_err()
            .contains(reason));
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), base);
        assert!(stub.calls.borrow().is_empty());
        assert!(!root.join("dist").exists());
    }
    assert!(release.publish("fixture/fstack", &BTreeMap::new()).is_err());
}
#[test]
fn draft_ownership_tag_identity_and_archive_checksum() {
    let (_temp, root) = fixture();
    let stub = stub();
    let release = Release {
        root: &root,
        runner: &stub,
        artifacts: None,
    };
    release.prepare(Some("0.2.1")).unwrap();
    commit(&root, "chore(release): v0.2.1");
    let sha = git(&root, &["rev-parse", "HEAD"]);
    let artifacts = artifacts(&root, &sha);
    let release = Release {
        artifacts: Some(&artifacts),
        ..release
    };
    origin(&root);
    let mut record = json!({"tag_name":"v0.2.1","draft":true,"body":format!("<!-- release-source:{sha} -->"),"author":{"login":"human"}});
    assert!(release
        .finish("fixture/fstack", "0.2.1", &[record.clone()], &sha)
        .unwrap_err()
        .contains("не принадлежит"));
    assert!(stub.calls.borrow().is_empty());
    assert!(!root.join("dist").exists());
    record["author"]["login"] = json!("github-actions[bot]");
    release
        .finish("fixture/fstack", "0.2.1", &[record], &sha)
        .unwrap();
    assert_eq!(
        stub.calls
            .borrow()
            .iter()
            .map(|v| v[2].as_str())
            .collect::<Vec<_>>(),
        ["upload", "edit"]
    );
    let path = root.join("dist/fstack-0.2.1-x86_64-unknown-linux-gnu.zip");
    let mut archive = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(
        archive
            .by_name("fstack/bin/fstack")
            .unwrap()
            .unix_mode()
            .unwrap()
            & 0o777,
        0o755
    );
    let bytes = fs::read(&path).unwrap();
    let checksum = fs::read_to_string(path.with_extension("zip.sha256")).unwrap();
    assert_eq!(
        checksum.split_whitespace().next().unwrap(),
        format!("{:x}", Sha256::digest(bytes))
    );
    stub.calls.borrow_mut().clear();
    save(&root, "other.txt", "Different revision");
    commit(&root, "chore: other");
    git(&root, &["tag", "v0.2.1"]);
    assert!(release
        .finish("fixture/fstack", "0.2.1", &[], &sha)
        .unwrap_err()
        .contains("другую ревизию"));
    assert!(stub.calls.borrow().is_empty());
}
#[test]
fn retry_resumes_owned_draft_even_after_docs_only_push() {
    let (_temp, root) = fixture();
    let mut stub = stub();
    stub.publish_tag = true;
    save(&root, "tools/fstack/src/runtime.rs", b"runtime one");
    save(
        &root,
        MANIFEST,
        updated_manifest(manifest(&root).unwrap(), "0.2.1")
            .unwrap()
            .unwrap(),
    );
    save(&root, PACKAGE_VERSION, "0.2.1\n");
    commit(&root, "fix: revise runtime");
    let pins = Release {
        root: &root,
        runner: &stub,
        artifacts: None,
    };
    save(
        &root,
        RUNTIME_ID,
        format!("{}\n", pins.runtime_id("HEAD").unwrap()),
    );
    git(&root, &["add", RUNTIME_ID]);
    git(&root, &["commit", "--amend", "--no-edit"]);
    let source = git(&root, &["rev-parse", "HEAD"]);
    let record = json!({"tag_name":"v0.2.1","draft":true,"prerelease":true,"target_commitish":"main",
        "author":{"login":"github-actions[bot]"},"body":format!("<!-- release-source:{source} -->")});
    stub.records.borrow_mut().push(record);
    save(&root, "guide.md", b"clarification");
    commit(&root, "docs: clarify setup");
    let head = git(&root, &["rev-parse", "HEAD"]);
    origin(&root);
    let artifacts = artifacts(&root, &head);
    let release = Release {
        root: &root,
        runner: &stub,
        artifacts: Some(&artifacts),
    };
    stub.fail_release_action.set(Some("edit"));
    assert!(release.publish("fixture/fstack", &event(&head)).is_err());
    stub.calls.borrow_mut().clear();
    stub.fail_release_action.set(None);
    release.publish("fixture/fstack", &event(&head)).unwrap();
    assert_eq!(
        stub.calls
            .borrow()
            .iter()
            .map(|v| v[2].as_str())
            .collect::<Vec<_>>(),
        ["upload", "edit"]
    );
    assert_eq!(git(&root, &["rev-parse", "v0.2.1"]), source);
    assert_eq!(stub.records.borrow()[1]["target_commitish"], source);
    assert_eq!(stub.records.borrow()[1]["prerelease"], false);
    stub.calls.borrow_mut().clear();
    release.publish("fixture/fstack", &event(&head)).unwrap();
    assert!(stub.calls.borrow().is_empty());
}
#[test]
fn publish_archives_exact_skill_bytes_and_rejects_untrusted_artifacts() {
    use std::io::Read;
    let (_temp, root) = fixture();
    save(
        &root,
        "skills/alpha/reference.md",
        b"Independent skill resource\n",
    );
    save(
        &root,
        MANIFEST,
        updated_manifest(manifest(&root).unwrap(), "0.3.0")
            .unwrap()
            .unwrap(),
    );
    save(&root, PACKAGE_VERSION, "0.3.0\n");
    commit(&root, "feat: validated behavior");
    let version_commit = git(&root, &["rev-parse", "HEAD"]);
    save(&root, "guide.md", "Documentation after the version change");
    commit(&root, "docs: clarify usage");
    let sha = git(&root, &["rev-parse", "HEAD"]);
    origin(&root);
    let artifacts = artifacts(&root, &sha);
    let stub = stub();
    *stub.version_pull.borrow_mut() = Some((
        version_commit,
        json!({
            "merged_at":"2026-11-17", "base":{"ref":"main","repo":{"full_name":"fixture/fstack"}},
            "body":"## При обновлении\n\n<!-- Подсказка -->\nЗамените старый вызов.\n\n## Проверка\nЛоги"
        }),
    ));
    let release = Release {
        root: &root,
        runner: &stub,
        artifacts: Some(&artifacts),
    };
    let first = artifacts.join(TARGETS[0]).join("fstack");
    let bytes = fs::read(&first).unwrap();
    fs::write(&first, b"Modified binary").unwrap();
    assert!(release
        .publish("fixture/fstack", &event(&sha))
        .unwrap_err()
        .contains("Контрольная сумма"));
    assert!(stub.calls.borrow().is_empty());
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), sha);
    assert!(!root.join("dist").exists());
    fs::write(first, bytes).unwrap();
    stub.fail_release_action.set(Some("create"));
    assert!(release.publish("fixture/fstack", &event(&sha)).is_err());
    assert!(remote_marketplace(&root).is_empty());
    stub.calls.borrow_mut().clear();
    stub.fail_release_action.set(None);
    git(&root, &["checkout", "--detach", &sha]);
    release.publish("fixture/fstack", &event(&sha)).unwrap();
    assert_eq!(*stub.created_notes.borrow(), format!(
        "Пакет fstack 0.3.0. Архивы, бинарники и SHA-256.\n\n## При обновлении\n\nЗамените старый вызов.\n\n<!-- release-source:{sha} -->"
    ));
    assert_eq!(
        stub.calls
            .borrow()
            .iter()
            .map(|v| v[2].as_str())
            .collect::<Vec<_>>(),
        ["create", "upload", "edit"]
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), sha);
    assert_eq!(check_versions(&root).unwrap(), "0.3.0");
    for target in TARGETS {
        let binary = format!("bin/{}", binary_name(target));
        let path = root.join(format!("dist/fstack-0.3.0-{target}.zip"));
        let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
        let mut bytes = Vec::new();
        archive
            .by_name("fstack/skills/alpha/reference.md")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"Independent skill resource\n");
        bytes.clear();
        archive
            .by_name("fstack/skills/alpha/SKILL.md")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, fs::read(root.join("skills/alpha/SKILL.md")).unwrap());
        let mut text = String::new();
        archive
            .by_name("fstack/.codex-plugin/plugin.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let data: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(data["version"], "0.3.0");
        let mut binary_bytes = Vec::new();
        archive
            .by_name(&format!("fstack/{binary}"))
            .unwrap()
            .read_to_end(&mut binary_bytes)
            .unwrap();
        let runtime_id = release.check_runtime_id(&sha).unwrap();
        let asset = root.join(format!(
            "dist/fstack-0.3.0-{runtime_id}-{target}{}",
            if target.contains("windows") {
                ".exe"
            } else {
                ""
            }
        ));
        assert_eq!(fs::read(&asset).unwrap(), binary_bytes);
        assert_eq!(
            fs::read_to_string(asset.with_file_name(format!(
                "{}.sha256",
                asset.file_name().unwrap().to_string_lossy()
            )))
            .unwrap(),
            format!(
                "{:x}  {}\n",
                Sha256::digest(&binary_bytes),
                asset.file_name().unwrap().to_string_lossy()
            )
        );
    }
    assert!(remote_marketplace(&root).is_empty());
    let head = git(&root, &["rev-parse", "HEAD"]);
    save(&root, "unrelated.md", "New behavior");
    commit(&root, "fix: later change");
    let later = git(&root, &["rev-parse", "HEAD"]);
    let other = self::artifacts(&root, &later);
    let old = Release {
        root: &root,
        runner: &stub,
        artifacts: Some(&other),
    };
    let count = stub.calls.borrow().len();
    assert!(old
        .finish("fixture/fstack", "0.3.0", &[], &head)
        .unwrap_err()
        .contains("Исходная ревизия"));
    assert_eq!(stub.calls.borrow().len(), count);
    fs::remove_dir_all(other.join(TARGETS[0])).unwrap();
    assert!(old.artifacts(&later).unwrap_err().contains("полный набор"));
}

#[test]
fn explicit_version_checks_package_and_skill_changes_against_the_pr_base() {
    let (_temp, root) = fixture();
    let release = Release {
        root: &root,
        runner: &System,
        artifacts: None,
    };
    let base = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(release.check_next(&base).unwrap(), "0.2.0");
    for invalid in ["1.0.0", "1.2", "00.2.3", "0.2.3-rc1", "0.2.3+build"] {
        assert!(release.prepare(Some(invalid)).is_err(), "{invalid}");
    }

    for (path, text) in [
        (
            "skills/alpha/SKILL.md",
            Some(
                "---\nname: alpha\ndescription: Fixture skill\ndisable-model-invocation: true\n---\nNew instruction\n",
            ),
        ),
        (
            "skills/alpha/SKILL_en.md",
            Some("---\nname: alpha\ndescription: Localized skill\n---\n"),
        ),
        ("skills/alpha/explanation.md", Some("New resource\n")),
        ("docs/skills.md", Some("Updated skill catalog")),
        ("assets/logo.png", Some("Replaced logo")),
        ("media/icon.png", Some("Composer icon declared outside assets")),
        ("gallery/shot.png", Some("Screenshot declared outside assets")),
        (".agents/plugins/marketplace.json", Some("Updated marketplace")),
        ("skills/beta/SKILL.md", None),
        (
            "skills/gamma/SKILL.md",
            Some("---\nname: gamma\ndescription: Added skill\n---\n"),
        ),
    ] {
        let previous = git(&root, &["rev-parse", "HEAD"]);
        if let Some(text) = text {
            save(&root, path, text);
        } else {
            fs::remove_file(root.join(path)).unwrap();
        }
        commit(&root, "fix: change shipped skill files");
        assert_eq!(
            release.check_next(&previous).unwrap_err(),
            "Изменение пакета требует новой версии",
            "{path}"
        );
    }
    release.prepare(Some("0.2.1")).unwrap();
    commit(&root, "chore: prepare package");
    assert_eq!(release.check_next(&base).unwrap(), "0.2.1");
    git(&root, &["tag", "v0.2.1"]);
    assert!(release.check_next(&base).unwrap_err().contains("занята"));
}

#[test]
fn exact_release_survives_a_missing_list_entry_and_git_tag() {
    let (_temp, root) = fixture();
    let stub = stub();
    let release = Release {
        root: &root,
        runner: &stub,
        artifacts: None,
    };
    let published = json!({"node_id":"R_1","tag_name":"v0.2.0","draft":false,"prerelease":false});
    *stub.exact.borrow_mut() = published.clone();
    *stub.records.borrow_mut() = vec![published];
    let records = release.records("fixture/fstack").unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(release.plan(&records).unwrap()["publish"], false);

    stub.records.borrow_mut().clear();
    git(&root, &["tag", "-d", "v0.2.0"]);
    let records = release.records("fixture/fstack").unwrap();
    assert!(release
        .plan(&records)
        .unwrap_err()
        .contains("Git-тег отсутствует"));
    assert!(stub.calls.borrow().is_empty());
}

#[test]
fn publishing_depends_on_the_current_version_only() {
    let (_temp, root) = fixture();
    let release = Release {
        root: &root,
        runner: &System,
        artifacts: None,
    };
    let base = git(&root, &["rev-parse", "HEAD"]);
    let same = json!({"tag_name":"v0.2.0","draft":false,"prerelease":false});
    let unrelated = json!({"tag_name":"v20.0.0","draft":false,"prerelease":false});
    assert_eq!(
        release.plan(std::slice::from_ref(&unrelated)).unwrap()["publish"],
        true
    );
    assert_eq!(release.plan(&[]).unwrap()["publish"], true);
    assert_eq!(
        release.plan(&[same.clone(), unrelated]).unwrap()["publish"],
        false
    );
    save(&root, "skills/alpha/explanation.md", "new resource\n");
    commit(&root, "fix: changed shipped skill");
    assert_eq!(
        release.plan(&[same]).unwrap_err(),
        "Версия 0.2.0 уже опубликована с другими исходниками"
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD^"]), base);
}

#[test]
fn release_preparation_binds_the_staged_build_inputs() {
    let (_temp, root) = fixture();
    let release = Release {
        root: &root,
        runner: &System,
        artifacts: None,
    };
    let base = git(&root, &["rev-parse", "HEAD"]);
    let input = "tools/fstack/build.rs";
    save(&root, input, "first");
    git(&root, &["add", input]);
    release.prepare(Some("0.2.1")).unwrap();
    save(&root, input, "second");
    commit(&root, "fix: change build after preparation");
    assert!(release
        .check_next(&base)
        .unwrap_err()
        .contains("не соответствует исходникам"));
    release.prepare(None).unwrap();
    commit(&root, "chore: refresh package");
    assert_eq!(release.check_next(&base).unwrap(), "0.2.1");
}
