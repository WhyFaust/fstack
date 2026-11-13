use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, String>;
const MANIFEST: &str = ".codex-plugin/plugin.json";
const PACKAGE_VERSION: &str = "launchers/package-version";
const RUNTIME_ID: &str = "launchers/runtime-id";
const BUILD_INPUTS: &[&str] = &[
    ".cargo",
    ".gitattributes",
    ".github/workflows/release.yml",
    "hooks",
    "launchers",
    "tools/fstack",
];
fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}
fn version(value: &str) -> Result<[u64; 3]> {
    if !rx(r"\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z").is_match(value) {
        return Err(format!("Некорректная версия: {value}"));
    }
    let parts: Vec<_> = value
        .split('.')
        .map(|n| n.parse::<u64>().map_err(|e| e.to_string()))
        .collect::<Result<_>>()?;
    Ok([parts[0], parts[1], parts[2]])
}
fn increment(value: &str, level: u8) -> Result<String> {
    let [a, b, c] = version(value)?;
    match level {
        0 => Ok(value.into()),
        1 => Ok(format!(
            "{a}.{b}.{}",
            c.checked_add(1).ok_or("Версия переполнена")?
        )),
        2 => Ok(format!(
            "{a}.{}.0",
            b.checked_add(1).ok_or("Версия переполнена")?
        )),
        3 => Ok(format!(
            "{}.0.0",
            a.checked_add(1).ok_or("Версия переполнена")?
        )),
        _ => Err("Некорректный уровень".into()),
    }
}
fn change_level(messages: &str) -> u8 {
    let header = rx(r"\A([a-zA-Z]+)(?:\([^\r\n)]+\))?(!)?:[ \t]+\S");
    messages
        .split('\0')
        .map(|message| {
            let parsed = header.captures(message.trim());
            if parsed.as_ref().is_some_and(|c| c.get(2).is_some())
                || rx(r"(?m)^BREAKING(?: |-)CHANGE:[ \t]+\S").is_match(message)
            {
                3
            } else {
                match parsed
                    .as_ref()
                    .map(|c| c[1].to_ascii_lowercase())
                    .as_deref()
                {
                    Some("feat") => 2,
                    Some("fix" | "perf") => 1,
                    _ => 0,
                }
            }
        })
        .max()
        .unwrap_or(0)
}
fn parse_skill(text: &str) -> Result<(serde_yaml::Value, String)> {
    let text = text.replace("\r\n", "\n");
    let matched = rx(r"\A---\n(?s:(.*?))\n---(?:\n|\z)");
    let capture = matched.captures(&text).ok_or("Нет frontmatter")?;
    let data: serde_yaml::Value = serde_yaml::from_str(&capture[1]).map_err(|e| e.to_string())?;
    if data["metadata"].as_mapping().is_none() {
        return Err("Некорректные metadata".into());
    }
    Ok((data, text[capture.get(0).unwrap().end()..].into()))
}
fn manifest(root: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(root.join(MANIFEST)).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn updated_manifest(mut data: Value, number: &str) -> Result<Option<Vec<u8>>> {
    version(number)?;
    if data["version"] == number {
        return Ok(None);
    }
    data["version"] = json!(number);
    Ok(Some(
        (serde_json::to_string_pretty(&data).map_err(|e| e.to_string())? + "\n").into_bytes(),
    ))
}
pub(super) fn check_package_version(root: &Path, number: &str) -> Result<()> {
    let path = crate::contained(root, root, PACKAGE_VERSION, false)?;
    if fs::read(path).map_err(|e| e.to_string())? != format!("{number}\n").as_bytes() {
        return Err(format!(
            "Версия запускателя {PACKAGE_VERSION}: ожидается {number}; выполните release prepare"
        ));
    }
    Ok(())
}
fn check_versions(root: &Path) -> Result<String> {
    let data = manifest(root)?;
    let current = data["version"].as_str().ok_or("Нет версии")?;
    version(current)?;
    check_package_version(root, current)?;
    let mut count = 0;
    for folder in fs::read_dir(root.join("skills")).map_err(|e| e.to_string())? {
        let folder = folder.map_err(|e| e.to_string())?.path();
        if !folder.is_dir() {
            continue;
        }
        let mut previous: Option<String> = None;
        for entry in fs::read_dir(folder).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !path
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.starts_with("SKILL") && v.ends_with(".md"))
            {
                continue;
            }
            let (data, _) = parse_skill(&fs::read_to_string(&path).map_err(|e| e.to_string())?)?;
            let number = data["metadata"]["version"]
                .as_str()
                .ok_or("Некорректная версия навыка")?;
            version(number)?;
            if previous.as_deref().is_some_and(|v| v != number) {
                return Err(format!(
                    "Версии локализаций навыка различаются: {}",
                    path.display()
                ));
            }
            previous = Some(number.into());
            count += 1;
        }
    }
    if count == 0 {
        return Err("Не найдены навыки".into());
    }
    Ok(current.into())
}
trait Runner {
    fn run(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>>;
    fn check(&self, root: &Path) -> Result<()> {
        crate::check(root).map(|_| ())
    }
}
struct System;
impl Runner for System {
    fn run(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut command = Command::new(args[0]);
        command.args(&args[1..]).current_dir(root);
        if let Some(input) = input {
            let mut file = tempfile::tempfile().map_err(|e| e.to_string())?;
            file.write_all(input).map_err(|e| e.to_string())?;
            file.rewind().map_err(|e| e.to_string())?;
            command.stdin(Stdio::from(file));
        }
        let output = command.output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "{}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }
}
struct Release<'a> {
    root: &'a Path,
    runner: &'a dyn Runner,
    artifacts: Option<&'a Path>,
}
impl Release<'_> {
    fn bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.runner.run(self.root, args, None)
    }
    fn cmd(&self, args: &[&str]) -> Result<String> {
        self.command(args, None)
    }
    fn command(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        String::from_utf8(self.runner.run(self.root, args, input)?)
            .map(|v| v.trim().to_owned())
            .map_err(|e| e.to_string())
    }
    fn live_main(&self) -> Result<String> {
        self.cmd(&[
            "git",
            "ls-remote",
            "--exit-code",
            "origin",
            "refs/heads/main",
        ])?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "Не найдена удалённая main".into())
    }
    fn check_live_main(&self, sha: &str) -> Result<()> {
        if self.live_main()? != sha {
            return Err("main изменился перед публикацией релиза".into());
        }
        Ok(())
    }
    fn records(&self, repo: &str) -> Result<Vec<Value>> {
        let pages: Vec<Vec<Value>> = serde_json::from_str(&self.cmd(&[
            "gh",
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/releases?per_page=100"),
        ])?)
        .map_err(|e| e.to_string())?;
        Ok(pages.into_iter().flatten().collect())
    }
    fn tree(&self, rev: &str) -> Result<BTreeMap<String, String>> {
        self.cmd(&["git", "ls-tree", "-r", "-z", rev, "--", "skills"])?
            .split('\0')
            .filter(|v| !v.is_empty())
            .map(|entry| {
                let (metadata, path) = entry.split_once('\t').ok_or("Некорректное дерево git")?;
                Ok((path.into(), metadata.into()))
            })
            .collect()
    }
    fn skill_at(&self, rev: &str, path: &str) -> Result<(serde_yaml::Value, String)> {
        let text = String::from_utf8(self.bytes(&["git", "show", &format!("{rev}:{path}")])?)
            .map_err(|e| e.to_string())?;
        parse_skill(&text)
    }
    fn skill_version(&self, rev: &str, path: &str) -> Result<[u64; 3]> {
        let (data, _) = self.skill_at(rev, path)?;
        version(
            data["metadata"]["version"]
                .as_str()
                .ok_or_else(|| format!("{path}: нет версии навыка"))?,
        )
    }
    fn signature(&self, rev: &str, path: &str) -> Result<(serde_yaml::Value, String)> {
        let (mut data, body) = self.skill_at(rev, path)?;
        data["metadata"]
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::String("version".into()));
        Ok((data, body.trim_matches('\n').to_owned()))
    }
    fn changed_skills(
        &self,
        base: &str,
        head: &str,
        release_level: Option<u8>,
    ) -> Result<Vec<String>> {
        let old = self.tree(base)?;
        let new = self.tree(head)?;
        let names: BTreeSet<_> = old
            .keys()
            .chain(new.keys())
            .filter_map(|p| p.strip_prefix("skills/")?.strip_suffix("/SKILL.md"))
            .filter(|name| !name.contains('/'))
            .collect();
        let mut changed = Vec::new();
        for name in names {
            let prefix = format!("skills/{name}/");
            let before: BTreeMap<_, _> =
                old.iter().filter(|(p, _)| p.starts_with(&prefix)).collect();
            let after: BTreeMap<_, _> =
                new.iter().filter(|(p, _)| p.starts_with(&prefix)).collect();
            if before == after {
                continue;
            }
            let entry = format!("{prefix}SKILL.md");
            let mut different = before.keys().ne(after.keys());
            if !different {
                for (path, metadata) in &after {
                    let previous = before[path];
                    if previous == *metadata {
                        continue;
                    }
                    let versioned = rx(r"\Askills/[^/]+/SKILL[^/]*\.md\z").is_match(path);
                    if previous
                        .split_whitespace()
                        .take(2)
                        .eq(metadata.split_whitespace().take(2))
                        && versioned
                        && self.signature(base, path)? == self.signature(head, path)?
                    {
                        continue;
                    }
                    different = true;
                    break;
                }
            }
            if let Some(level) = release_level {
                let before = old
                    .contains_key(&entry)
                    .then(|| self.skill_version(base, &entry))
                    .transpose()?;
                let after = new
                    .contains_key(&entry)
                    .then(|| self.skill_version(head, &entry))
                    .transpose()?;
                match (before, after) {
                    (None, Some(next)) if next != [1, 0, 0] => {
                        return Err(format!("{name}: начальная версия навыка должна быть 1.0.0"));
                    }
                    (Some(previous), Some(next))
                        if next < previous || (different && next == previous) =>
                    {
                        return Err(format!(
                            "{name}: требуется повышение версии изменённого навыка"
                        ));
                    }
                    _ => {}
                }
                if level == 0 && (different || before != after) {
                    return Err(format!("{name}: изменение навыка требует выпуска пакета"));
                }
            }
            if different {
                changed.push(name.to_owned());
            }
        }
        Ok(changed)
    }
    fn plan(&self, records: Vec<Value>) -> Result<Value> {
        let latest = records
            .iter()
            .filter(|r| r["draft"] == false && r["prerelease"] == false)
            .filter_map(|r| {
                r["tag_name"].as_str().and_then(|t| {
                    t.strip_prefix('v')
                        .and_then(|v| version(v).ok().map(|n| (n, t, v)))
                })
            })
            .max_by_key(|r| r.0);
        let (base, number) = if let Some((_, tag, v)) = latest {
            self.cmd(&["git", "merge-base", "--is-ancestor", tag, "HEAD"])?;
            (tag.to_owned(), v.to_owned())
        } else {
            let additions = self.cmd(&[
                "git",
                "log",
                "--diff-filter=A",
                "--format=%H",
                "--",
                ".github/workflows/release.yml",
            ])?;
            let first = additions
                .lines()
                .last()
                .ok_or("Сначала закоммитьте workflow релиза")?;
            let base = self.cmd(&["git", "rev-parse", &format!("{first}^")])?;
            let data: Value =
                serde_json::from_str(&self.cmd(&["git", "show", &format!("{base}:{MANIFEST}")])?)
                    .map_err(|e| e.to_string())?;
            (base, data["version"].as_str().ok_or("Нет версии")?.into())
        };
        let current = check_versions(self.root)?;
        let published = version(&number)?;
        let reserved = version(&current)?;
        if reserved < published {
            return Err(format!(
                "Версия {current} предшествует опубликованной {number}"
            ));
        }
        self.check_runtime_id("HEAD")?;
        let history = self.cmd(&[
            "git",
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H",
            &format!("{base}..HEAD"),
            "--",
            MANIFEST,
        ])?;
        let mut reservations = Vec::new();
        let mut previous = number.clone();
        for commit in history.lines() {
            let data = self.manifest_at(commit)?;
            let next = data["version"].as_str().ok_or("Нет версии")?;
            if next == previous {
                continue;
            }
            reservations.push((commit, next.to_owned()));
            previous = next.to_owned();
        }
        let mut parent = base.as_str();
        let mut previous = number.as_str();
        for pair in reservations.windows(2) {
            let (commit, next) = &pair[0];
            self.check_reservation(parent, commit, previous, next)?;
            parent = commit;
            previous = next;
        }
        self.check_reservation(parent, "HEAD", previous, &current)?;
        let tag = format!("v{current}");
        for record in records
            .iter()
            .filter(|record| record["draft"] == true && record["tag_name"] == tag)
        {
            self.draft_target(record, "HEAD", &current)?;
        }
        let level = match (published, reserved) {
            ([a, _, _], [b, _, _]) if a != b => 3,
            ([_, a, _], [_, b, _]) if a != b => 2,
            ([_, _, a], [_, _, b]) if a != b => 1,
            _ => 0,
        };
        Ok(
            json!({"version":current,"level":level,"base":base,"skills":self.changed_skills(&base, "HEAD", None)?,"records":records}),
        )
    }
    fn check_reservation(
        &self,
        base: &str,
        head: &str,
        previous: &str,
        reserved: &str,
    ) -> Result<()> {
        let level = self.release_level(base, head, "")?;
        let expected = self.version_for_tree(base, head, previous, level)?;
        if reserved != expected {
            return Err(format!(
                "Манифест {head}: ожидается {expected}, получено {reserved}"
            ));
        }
        Ok(())
    }
    fn release_level(&self, base: &str, head: &str, title: &str) -> Result<u8> {
        let commits_level = self.change_level_since(base, head)?;
        let level = if title.is_empty() {
            commits_level
        } else {
            change_level(title)
        };
        if !title.is_empty() && commits_level != level {
            return Err("Заголовок PR и коммиты ветки должны задавать одинаковый уровень Conventional Commits".into());
        }
        Ok(level)
    }
    fn version_for_tree(
        &self,
        base: &str,
        head: &str,
        previous: &str,
        level: u8,
    ) -> Result<String> {
        if level == 0 && !self.same_build_inputs(base, head)? {
            return Err(
                "Изменение runtime, хуков или сборки требует fix:, feat: или breaking change"
                    .into(),
            );
        }
        self.changed_skills(base, head, Some(level))?;
        increment(previous, level)
    }
    fn prepare_manifest(&self, base: &str, title: &str, write: bool) -> Result<String> {
        let base = self.cmd(&[
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ])?;
        self.cmd(&["git", "merge-base", "--is-ancestor", &base, "HEAD"])?;
        let parent = self.manifest_at(&base)?;
        let inputs = if write {
            self.cmd(&["git", "write-tree"])?
        } else {
            "HEAD".into()
        };
        let level = self.release_level(&base, "HEAD", title)?;
        let expected = self.version_for_tree(
            &base,
            &inputs,
            parent["version"].as_str().ok_or("Нет версии")?,
            level,
        )?;
        if write {
            let runtime_id = self.runtime_id(&inputs)?;
            fs::write(self.root.join(PACKAGE_VERSION), format!("{expected}\n"))
                .map_err(|e| e.to_string())?;
            fs::write(self.root.join(RUNTIME_ID), format!("{runtime_id}\n"))
                .map_err(|e| e.to_string())?;
        } else {
            self.check_runtime_id("HEAD")?;
            check_package_version(self.root, &expected)?;
        }
        if let Some(bytes) = self.version_contents(&expected)? {
            if !write {
                return Err(format!("В PR требуется версия {expected}; выполните release prepare для его базы и заголовка"));
            }
            fs::write(self.root.join(MANIFEST), bytes).map_err(|e| e.to_string())?;
        }
        Ok(expected)
    }
    fn change_level_since(&self, base: &str, head: &str) -> Result<u8> {
        Ok(change_level(&self.cmd(&[
            "git",
            "log",
            "--format=%B%x00",
            &format!("{base}..{head}"),
        ])?))
    }
    fn manifest_at(&self, revision: &str) -> Result<Value> {
        serde_json::from_slice(&self.bytes(&["git", "show", &format!("{revision}:{MANIFEST}")])?)
            .map_err(|e| e.to_string())
    }
    fn version_contents(&self, number: &str) -> Result<Option<Vec<u8>>> {
        updated_manifest(manifest(self.root)?, number)
    }
    fn artifacts(&self, sha: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let directory = self.artifacts.ok_or("Публикация требует --artifacts DIR")?;
        let mut found = BTreeMap::new();
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !path.is_dir()
                || fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_symlink()
            {
                return Err("Некорректный каталог артефакта".into());
            }
            let descriptor = path.join("artifact.json");
            if fs::symlink_metadata(&descriptor)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Ссылка вместо описания артефакта".into());
            }
            let data: Value =
                serde_json::from_slice(&fs::read(descriptor).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let target = data["target"].as_str().ok_or("Нет target артефакта")?;
            if !TARGETS.contains(&target)
                || path.file_name().and_then(|v| v.to_str()) != Some(target)
                || found.contains_key(target)
            {
                return Err("Неизвестный или повторный target артефакта".into());
            }
            let source = data["source_sha"]
                .as_str()
                .ok_or("Нет source_sha артефакта")?;
            if !rx(r"\A[a-f0-9]{40}\z").is_match(source) {
                return Err("Некорректный source_sha артефакта".into());
            }
            if source != sha {
                return Err("Исходная ревизия бинарного артефакта не совпадает с релизом".into());
            }
            let binary = binary_name(target);
            if data["binary"] != binary {
                return Err("Некорректное имя бинарного артефакта".into());
            }
            let file = path.join(binary);
            if fs::symlink_metadata(&file)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Ссылка вместо бинарного артефакта".into());
            }
            let bytes = fs::read(file).map_err(|e| e.to_string())?;
            if bytes.is_empty() || data["sha256"] != format!("{:x}", Sha256::digest(&bytes)) {
                return Err("Контрольная сумма бинарного артефакта не совпадает".into());
            }
            found.insert(target.to_owned(), bytes);
        }
        if found.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != TARGETS.into_iter().collect()
        {
            return Err("Требуется полный набор поддерживаемых платформ".into());
        }
        Ok(found.into_iter().collect())
    }
    fn archive(
        &self,
        number: &str,
        sha: &str,
        artifacts: &[(String, Vec<u8>)],
    ) -> Result<Vec<PathBuf>> {
        let data = manifest(self.root)?;
        let name = data["name"].as_str().ok_or("Нет имени пакета")?;
        if !rx(r"\A[a-zA-Z0-9_-]+\z").is_match(name) {
            return Err("Некорректное имя пакета".into());
        }
        let runtime_id = self.check_runtime_id(sha)?;
        let mut outputs = Vec::new();
        for (target, bytes) in artifacts {
            let output = self
                .root
                .join("dist")
                .join(format!("{name}-{number}-{target}.zip"));
            fs::create_dir_all(output.parent().unwrap()).map_err(|e| e.to_string())?;
            self.cmd(&[
                "git",
                "-c",
                "core.autocrlf=false",
                "archive",
                "--format=zip",
                &format!("--prefix={name}/"),
                &format!("--output={}", output.display()),
                sha,
            ])?;
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&output)
                .map_err(|e| e.to_string())?;
            let mut archive = zip::ZipWriter::new_append(file).map_err(|e| e.to_string())?;
            archive
                .start_file(
                    format!("{name}/bin/{}", binary_name(target)),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated)
                        .unix_permissions(0o755),
                )
                .map_err(|e| e.to_string())?;
            archive.write_all(bytes).map_err(|e| e.to_string())?;
            archive.finish().map_err(|e| e.to_string())?;
            let binary = output.with_file_name(format!(
                "{name}-{number}-{runtime_id}-{target}{}",
                if target.contains("windows") {
                    ".exe"
                } else {
                    ""
                }
            ));
            fs::write(&binary, bytes).map_err(|e| e.to_string())?;
            outputs.push(write_checksum(&binary, bytes)?);
            outputs.push(binary);
            let archive_bytes = fs::read(&output).map_err(|e| e.to_string())?;
            outputs.push(write_checksum(&output, &archive_bytes)?);
            outputs.push(output);
        }
        Ok(outputs)
    }
    fn draft_target(&self, record: &Value, head: &str, number: &str) -> Result<String> {
        let body = record["body"].as_str().unwrap_or("");
        let marker = rx(r"(?m)^<!-- release-source:([a-f0-9]{40}) -->$");
        let mut matches = marker.captures_iter(body);
        let target = matches.next().ok_or("Нет ревизии черновика")?[1].to_owned();
        if matches.next().is_some()
            || record["author"]["login"] != "github-actions[bot]"
            || record["draft"] != true
        {
            return Err("Черновик не принадлежит этому выпуску".into());
        }
        self.cmd(&["git", "merge-base", "--is-ancestor", &target, head])?;
        if self.manifest_at(&target)?["version"] != number
            || !self.same_build_inputs(&target, head)?
            || !self.changed_skills(&target, head, None)?.is_empty()
        {
            return Err("Исходники черновика отличаются от проверенного выпуска".into());
        }
        Ok(target)
    }
    fn runtime_id(&self, revision: &str) -> Result<String> {
        let mut args = vec!["git", "ls-tree", "-r", "-z", revision, "--"];
        args.extend_from_slice(BUILD_INPUTS);
        let tree = self.bytes(&args)?;
        let mut hash = Sha256::new();
        for entry in tree.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let path = entry
                .splitn(2, |b| *b == b'\t')
                .nth(1)
                .ok_or("Некорректная запись git ls-tree")?;
            if path != RUNTIME_ID.as_bytes() && path != PACKAGE_VERSION.as_bytes() {
                hash.update(entry);
                hash.update([0]);
            }
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    fn check_runtime_id(&self, revision: &str) -> Result<String> {
        let runtime_id = self.runtime_id(revision)?;
        let path = format!("{revision}:{RUNTIME_ID}");
        let declared = self.bytes(&["git", "show", &path])?;
        if declared != format!("{runtime_id}\n").as_bytes() {
            return Err(format!(
                "{RUNTIME_ID} не соответствует исходникам {revision}; выполните release prepare"
            ));
        }
        Ok(runtime_id)
    }
    fn same_build_inputs(&self, base: &str, head: &str) -> Result<bool> {
        Ok(self.runtime_id(base)? == self.runtime_id(head)?)
    }
    fn finish(
        &self,
        repo: &str,
        number: &str,
        records: &[Value],
        base: &str,
        head: &str,
    ) -> Result<()> {
        version(number)?;
        let tag = format!("v{number}");
        let existing = records.iter().find(|r| r["tag_name"] == tag);
        if existing.is_some_and(|r| r["draft"] != true) {
            return Err(format!("Релиз {tag} уже опубликован"));
        }
        let target = existing
            .map(|r| self.draft_target(r, head, number))
            .transpose()?;
        let sha = target.as_deref().unwrap_or(head);
        let marker = format!("<!-- release-source:{sha} -->");
        if !self.cmd(&["git", "tag", "--list", &tag])?.is_empty()
            && self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])? != sha
        {
            return Err(format!("Тег {tag} указывает на другую ревизию"));
        }
        self.check_live_main(head)?;
        let artifacts = self.artifacts(head)?;
        let outputs = self.archive(number, sha, &artifacts)?;
        if existing.is_none() {
            let entries = self.cmd(&[
                "git",
                "log",
                "--format=- %s (%h)",
                &format!("{base}..{sha}"),
            ])?;
            let notes = self.root.join("dist/release-notes.md");
            fs::write(&notes, format!("## Изменения\n\n{entries}\n\n{marker}\n"))
                .map_err(|e| e.to_string())?;
            self.cmd(&[
                "gh",
                "release",
                "create",
                &tag,
                "--repo",
                repo,
                "--target",
                sha,
                "--title",
                &tag,
                "--notes-file",
                &notes.to_string_lossy(),
                "--draft",
            ])?;
        }
        let output_names: Vec<_> = outputs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut upload = vec!["gh", "release", "upload", &tag];
        upload.extend(output_names.iter().map(String::as_str));
        upload.extend(["--repo", repo, "--clobber"]);
        self.cmd(&upload)?;
        self.check_live_main(head)?;
        self.cmd(&[
            "gh",
            "release",
            "edit",
            &tag,
            "--repo",
            repo,
            "--target",
            sha,
            "--draft=false",
            "--prerelease=false",
            "--latest",
        ])?;
        println!("{}", json!({"published":tag,"sha":sha}));
        Ok(())
    }
    fn publish(&self, repo: &str, event: &BTreeMap<String, String>) -> Result<()> {
        if event.get("GITHUB_ACTIONS").map(String::as_str) != Some("true")
            || event.get("GITHUB_REF").map(String::as_str) != Some("refs/heads/main")
        {
            return Err("Публикация выполняется только workflow ветки main".into());
        }
        if event.get("GITHUB_REPOSITORY").map(String::as_str) != Some(repo) {
            return Err("Репозиторий workflow не совпадает с целью".into());
        }
        let expected = event.get("GITHUB_SHA").map(String::as_str).unwrap_or("");
        if !rx(r"\A[a-f0-9]{40}\z").is_match(expected)
            || self.cmd(&["git", "rev-parse", "HEAD"])? != expected
        {
            return Err("Релиз должен начинаться с проверенного SHA события workflow".into());
        }
        if !self.cmd(&["git", "status", "--porcelain"])?.is_empty() {
            return Err("Рабочее дерево содержит изменения".into());
        }
        self.check_live_main(expected)?;
        let plan = self.plan(self.records(repo)?)?;
        if plan["level"] == 0 {
            println!("Нет изменений, требующих релиза.");
            return Ok(());
        }
        self.runner.check(self.root)?;
        let number = plan["version"].as_str().unwrap();
        self.finish(
            repo,
            number,
            plan["records"].as_array().unwrap(),
            plan["base"].as_str().unwrap(),
            expected,
        )
    }
}
const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];
fn write_checksum(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let filename = path.file_name().unwrap().to_string_lossy();
    let checksum = path.with_file_name(format!("{filename}.sha256"));
    let hash = format!("{:x}", Sha256::digest(bytes));
    fs::write(&checksum, format!("{hash}  {filename}\n")).map_err(|e| e.to_string())?;
    Ok(checksum)
}

fn binary_name(target: &str) -> &str {
    if target.contains("windows") {
        "fstack.exe"
    } else {
        "fstack"
    }
}
fn option<'a>(args: &'a [OsString], name: &str) -> Result<&'a std::ffi::OsStr> {
    let index = args
        .iter()
        .position(|v| v == name)
        .ok_or_else(|| format!("Требуется {name}"))?;
    args.get(index + 1)
        .map(OsString::as_os_str)
        .ok_or_else(|| format!("Требуется значение {name}"))
}
fn artifact(args: &[OsString], package: &Path) -> Result<()> {
    if args.len() != 6
        || args
            .chunks_exact(2)
            .any(|pair| !matches!(pair[0].to_str(), Some("--target" | "--binary" | "--output")))
    {
        return Err(
            "Использование: fstack release artifact --target TARGET --binary PATH --output DIR"
                .into(),
        );
    }
    let target = option(args, "--target")?
        .to_str()
        .ok_or("Некорректный target")?;
    if !TARGETS.contains(&target) {
        return Err("Неподдерживаемый target".into());
    }
    let binary = Path::new(option(args, "--binary")?);
    let output = Path::new(option(args, "--output")?).join(target);
    let release = Release {
        root: package,
        runner: &System,
        artifacts: None,
    };
    let sha = release.cmd(&["git", "rev-parse", "HEAD"])?;
    if !release.cmd(&["git", "status", "--porcelain"])?.is_empty() {
        return Err("Бинарный артефакт требует чистое рабочее дерево".into());
    }
    let bytes = fs::read(binary).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("Пустой бинарный артефакт".into());
    }
    let name = binary_name(target);
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    fs::write(output.join(name), &bytes).map_err(|e| e.to_string())?;
    let data = json!({"target":target,"source_sha":sha,"sha256":format!("{:x}",Sha256::digest(bytes)),"binary":name});
    fs::write(
        output.join("artifact.json"),
        serde_json::to_string_pretty(&data).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())?;
    println!("{}", output.display());
    Ok(())
}
pub fn run(args: &[OsString], package: &Path) -> Result<()> {
    let action = args.first().and_then(|v| v.to_str()).unwrap_or("");
    if action == "artifact" {
        return artifact(&args[1..], package);
    }
    if action == "check" && args.len() == 1 {
        println!("{}", check_versions(package)?);
        return Ok(());
    }
    if matches!(action, "prepare" | "check-next") && matches!(args.len(), 2 | 3) {
        let base = args[1].to_str().ok_or("Некорректная база PR")?;
        let title = args
            .get(2)
            .map(|s| s.to_str().ok_or("Некорректный заголовок PR"))
            .transpose()?
            .unwrap_or("");
        let release = Release {
            root: package,
            runner: &System,
            artifacts: None,
        };
        println!(
            "{}",
            release.prepare_manifest(base, title, action == "prepare")?
        );
        return Ok(());
    }
    let artifacts = if action == "publish" && args.len() == 3 && args[1] == "--artifacts" {
        Some(Path::new(&args[2]))
    } else if action == "plan" && args.len() == 1 {
        None
    } else {
        return Err("Использование: fstack release check|prepare BASE [TITLE]|check-next BASE [TITLE]|plan|publish --artifacts DIR|artifact --target TARGET --binary PATH --output DIR".into());
    };
    let release = Release {
        root: package,
        runner: &System,
        artifacts,
    };
    let repo = std::env::var("GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .map(Ok)
        .unwrap_or_else(|| {
            release.cmd(&[
                "gh",
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ])
        })?;
    if !rx(r"\A[\w.-]+/[\w.-]+\z").is_match(&repo) {
        return Err("Некорректное имя репозитория".into());
    }
    if action == "publish" {
        release.publish(&repo, &std::env::vars().collect())
    } else {
        let mut plan = release.plan(release.records(&repo)?)?;
        plan.as_object_mut().unwrap().remove("records");
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?
        );
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Stub {
        records: RefCell<Vec<Value>>,
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
                return serde_json::to_vec(&vec![self.records.borrow().clone()])
                    .map_err(|e| e.to_string());
            }
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            if args[1] == "release" && self.fail_release_action.get() == Some(args[2]) {
                return Err(format!("Сбой gh release {}", args[2]));
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
            b"{\"name\": \"fstack\", \"version\": \"0.2.0\"}",
        );
        save(&root, PACKAGE_VERSION, "0.2.0\n");
        save(&root, ".gitignore", b"dist/\n");
        for name in ["alpha", "beta"] {
            save(
                &root,
                &format!("skills/{name}/SKILL.md"),
                format!(
                    "---\nname: {name}\nmetadata:\n  version: 0.2.0\n---\n# Original body {name}\n"
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
    fn stable_versions_and_conventional_commits() {
        for invalid in ["01.2.0", "1.02.0", "1.2.03", "1.2", "1.2.3-alpha", "1.2.1١"] {
            assert!(version(invalid).is_err());
        }
        assert_eq!(
            (0..=3)
                .map(|level| increment("1.2.3", level).unwrap())
                .collect::<Vec<_>>(),
            ["1.2.3", "1.2.4", "1.3.0", "2.0.0"]
        );
        for (message, expected) in [
            ("docs: text", 0),
            ("fix(profile): text", 1),
            ("perf: text", 1),
            ("fix: text\0feat: text", 2),
            ("refactor!: text", 3),
            ("chore: text\n\nBREAKING CHANGE: contract", 3),
        ] {
            assert_eq!(change_level(message), expected);
        }
    }
    #[test]
    fn independent_localizations_and_final_tree_versions() {
        let (_temp, root) = fixture();
        let stub = stub();
        let release = Release {
            root: &root,
            runner: &stub,
            artifacts: None,
        };
        let path = root.join("skills/alpha/SKILL.md");
        let original = fs::read_to_string(&path).unwrap();
        let updated = original.replace("version: 0.2.0", "version: \"9.0.0\" # spelling");
        fs::write(&path, &updated).unwrap();
        commit(&root, "chore: version only");
        assert_eq!(
            release.changed_skills("v0.2.0", "HEAD", None).unwrap(),
            Vec::<String>::new()
        );
        save(&root, "skills/alpha/SKILL.en.md", &updated);
        assert_eq!(check_versions(&root).unwrap(), "0.2.0");
        save(&root, "skills/alpha/SKILL.en.md", &original);
        assert!(check_versions(&root).is_err());
        fs::remove_file(root.join("skills/alpha/SKILL.en.md")).unwrap();
        save(&root, "skills/alpha/references/run.md", b"New reference");
        save(&root, "skills/beta/assets/icon.bin", b"\x00\x01\xff");
        save(
            &root,
            "skills/gamma/SKILL.md",
            b"---\nname: gamma\nmetadata:\n  version: 1.0.0\n---\nNew skill\n",
        );
        let beta = root.join("skills/beta/SKILL.md");
        fs::write(
            &beta,
            fs::read_to_string(&beta).unwrap().replace("0.2.0", "0.2.1"),
        )
        .unwrap();
        commit(&root, "feat: resources");
        assert_eq!(
            release.changed_skills("v0.2.0", "HEAD", None).unwrap(),
            ["alpha", "beta", "gamma"]
        );
        fs::remove_file(root.join("skills/alpha/references/run.md")).unwrap();
        commit(&root, "refactor: undo resource");
        assert_eq!(
            release.changed_skills("v0.2.0", "HEAD", None).unwrap(),
            ["beta", "gamma"]
        );
        let skill_before = fs::read(&path).unwrap();
        assert!(release.prepare_manifest("v0.2.0", "", false).is_err());
        assert_eq!(
            release.prepare_manifest("v0.2.0", "", true).unwrap(),
            "0.3.0"
        );
        assert_eq!(fs::read(&path).unwrap(), skill_before);
        assert!(release.version_contents("0.3.0").unwrap().is_none());
        assert_eq!(
            release.prepare_manifest("v0.2.0", "", false).unwrap(),
            "0.3.0"
        );
    }
    #[test]
    fn skill_changes_require_both_a_skill_version_and_a_release() {
        let (_temp, root) = fixture();
        let stub = stub();
        let release = Release {
            root: &root,
            runner: &stub,
            artifacts: None,
        };
        let base = git(&root, &["rev-parse", "HEAD"]);
        let path = root.join("skills/alpha/SKILL.md");
        let original = fs::read_to_string(&path).unwrap();
        let formatted = original
            .replace("version: 0.2.0", "version: '0.2.0' # stable")
            .replace("---\n# Original", "---\n\n# Original")
            + "\n";
        fs::write(&path, formatted).unwrap();
        commit(&root, "docs: format skill metadata");
        assert_eq!(release.prepare_manifest(&base, "", false).unwrap(), "0.2.0");
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["level"],
            0
        );
        let changed = original.replace("# Original body alpha", "# Changed procedure");
        fs::write(&path, &changed).unwrap();
        commit(&root, "docs: clarify procedure");
        assert!(release
            .prepare_manifest(&base, "", false)
            .unwrap_err()
            .contains("повышение версии"));
        assert!(release
            .plan(stub.records.borrow().clone())
            .unwrap_err()
            .contains("повышение версии"));
        fs::write(&path, changed.replace("0.2.0", "0.2.1")).unwrap();
        git(&root, &["add", "skills"]);
        assert!(release
            .prepare_manifest(&base, "", true)
            .unwrap_err()
            .contains("выпуска пакета"));
        commit(&root, "fix: correct procedure");
        release.prepare_manifest(&base, "", true).unwrap();
        commit(&root, "chore: prepare release");
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "0.2.1"
        );
        save(&root, PACKAGE_VERSION, "0.2.0\n");
        assert!(check_versions(&root).is_err());
        assert!(release.prepare_manifest(&base, "", false).is_err());
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
        save(
            &root,
            MANIFEST,
            release.version_contents("0.2.1").unwrap().unwrap(),
        );
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
            .finish("fixture/fstack", "0.2.1", &[record.clone()], &sha, &sha)
            .unwrap_err()
            .contains("не принадлежит"));
        assert!(stub.calls.borrow().is_empty());
        assert!(!root.join("dist").exists());
        record["author"]["login"] = json!("github-actions[bot]");
        release
            .finish("fixture/fstack", "0.2.1", &[record], &sha, &sha)
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
            .finish("fixture/fstack", "0.2.1", &[], &sha, &sha)
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
        let alpha = root.join("skills/alpha/SKILL.md");
        fs::write(
            &alpha,
            fs::read_to_string(&alpha)
                .unwrap()
                .replace("0.2.0", "0.2.1"),
        )
        .unwrap();
        save(&root, PACKAGE_VERSION, "0.3.0\n");
        commit(&root, "feat: validated behavior");
        let sha = git(&root, &["rev-parse", "HEAD"]);
        origin(&root);
        let artifacts = artifacts(&root, &sha);
        let stub = stub();
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
            .finish("fixture/fstack", "0.3.0", &[], &sha, &head)
            .unwrap_err()
            .contains("Исходная ревизия"));
        assert_eq!(stub.calls.borrow().len(), count);
        fs::remove_dir_all(other.join(TARGETS[0])).unwrap();
        assert!(old.artifacts(&later).unwrap_err().contains("полный набор"));
    }
    #[test]
    fn independent_runtime_fixes_with_same_version_conflict_on_release_identity() {
        let (_temp, root) = fixture();
        let release = Release {
            root: &root,
            runner: &System,
            artifacts: None,
        };
        let base = git(&root, &["rev-parse", "HEAD"]);
        for (branch, path) in [
            ("first", "tools/fstack/src/one.rs"),
            ("second", ".cargo/config.toml"),
        ] {
            git(&root, &["checkout", "-b", branch, &base]);
            save(&root, path, b"input");
            commit(&root, "fix: revise build input");
            save(&root, path, b"final input");
            git(&root, &["add", path]);
            assert_eq!(
                release
                    .prepare_manifest(&base, "fix: revise build input", true)
                    .unwrap(),
                "0.2.1"
            );
            commit(&root, "chore: prepare release");
            assert_eq!(
                release.check_runtime_id("HEAD").unwrap(),
                fs::read_to_string(root.join(RUNTIME_ID))
                    .unwrap()
                    .trim_end()
            );
        }
        let first = release.check_runtime_id("first").unwrap();
        let second = release.check_runtime_id("second").unwrap();
        assert_ne!(first, second);
        assert!(release.cmd(&["git", "merge", "first"]).is_err());
        assert_eq!(
            git(&root, &["diff", "--name-only", "--diff-filter=U"]),
            RUNTIME_ID
        );
        save(&root, RUNTIME_ID, format!("{second}\n"));
        commit(&root, "fix: incorrectly resolve release identity");
        assert!(release.check_runtime_id("HEAD").is_err());
        assert!(release.plan(stub().records.into_inner()).is_err());
    }
    #[test]
    fn preview_merge_squash_and_rebase_accept_the_same_finished_change() {
        let (_temp, root) = fixture();
        let base = git(&root, &["rev-parse", "HEAD"]);
        let stub = stub();
        let release = Release {
            root: &root,
            runner: &stub,
            artifacts: None,
        };
        git(&root, &["checkout", "-b", "feature"]);
        save(&root, "tools/fstack/src/main.rs", b"new runtime");
        commit(&root, "feat!: new runtime protocol");
        assert!(release
            .prepare_manifest(&base, "fix: too small", false)
            .is_err());
        assert_eq!(
            release
                .prepare_manifest(&base, "feat!: new runtime protocol", true)
                .unwrap(),
            "1.0.0"
        );
        commit(&root, "chore: reserve package version");
        save(&root, "tools/fstack/src/main.rs", b"final runtime");
        git(&root, &["add", "tools/fstack/src/main.rs"]);
        release
            .prepare_manifest(&base, "feat!: new runtime protocol", true)
            .unwrap();
        commit(&root, "fix: address review");
        let feature_tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.0"
        );
        let feature = git(&root, &["rev-parse", "HEAD^"]);
        assert!(release
            .prepare_manifest(&feature, "feat: misleading title", false)
            .is_err());
        git(&root, &["checkout", "main"]);
        git(
            &root,
            &["merge", "--no-ff", "feature", "-m", "Merge pull request #1"],
        );
        assert_eq!(
            release
                .prepare_manifest(&base, "feat!: new runtime protocol", false)
                .unwrap(),
            "1.0.0"
        );
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.0"
        );
        git(&root, &["reset", "--hard", &base]);
        git(&root, &["merge", "--squash", "feature"]);
        git(&root, &["commit", "-m", "feat!: new runtime protocol"]);
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.0"
        );
        git(&root, &["reset", "--hard", &base]);
        git(&root, &["cherry-pick", &format!("{base}..feature")]);
        assert_eq!(git(&root, &["rev-parse", "HEAD^{tree}"]), feature_tree);
        origin(&root);
        let sha = git(&root, &["rev-parse", "HEAD"]);
        let artifacts = artifacts(&root, &sha);
        let release = Release {
            artifacts: Some(&artifacts),
            ..release
        };
        release.publish("fixture/fstack", &event(&sha)).unwrap();
        assert_eq!(stub.calls.borrow().last().unwrap()[2], "edit");
    }
    #[test]
    fn consecutive_unpublished_changes_require_distinct_runtime_versions() {
        let (_temp, root) = fixture();
        let stub = stub();
        let release = Release {
            root: &root,
            runner: &stub,
            artifacts: None,
        };
        let original = git(&root, &["rev-parse", "HEAD"]);
        save(&root, "tools/fstack/src/main.rs", b"first");
        commit(&root, "feat!: initial implementation");
        assert!(release
            .prepare_manifest(&original, "fix: too small", false)
            .is_err());
        assert_eq!(
            release
                .prepare_manifest(&original, "feat!: breaking rollout", true)
                .unwrap(),
            "1.0.0"
        );
        git(&root, &["reset", "--soft", &original]);
        commit(&root, "feat!: breaking rollout");
        let first = git(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.0"
        );
        let draft = json!({"tag_name":"v1.0.0","draft":true,"prerelease":false,
            "author":{"login":"github-actions[bot]"},"body":format!("<!-- release-source:{first} -->")});
        stub.records.borrow_mut().push(draft);
        save(&root, "tools/fstack/src/main.rs", b"second");
        commit(&root, "fix: runtime correction");
        assert!(release.prepare_manifest(&first, "", false).is_err());
        assert!(release.plan(stub.records.borrow().clone()).is_err());
        assert_eq!(release.prepare_manifest(&first, "", true).unwrap(), "1.0.1");
        git(&root, &["reset", "--soft", &first]);
        commit(&root, "fix: runtime correction");
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.1"
        );
        let second = git(&root, &["rev-parse", "HEAD"]);
        save(&root, "docs/usage.md", b"details");
        commit(&root, "docs: clarify usage");
        assert_eq!(
            release.prepare_manifest(&second, "", false).unwrap(),
            "1.0.1"
        );
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "1.0.1"
        );
        save(&root, "hooks/hook.sh", b"new runtime input");
        commit(&root, "docs: wrong classification");
        assert!(release.prepare_manifest(&second, "", false).is_err());
        assert!(release.plan(stub.records.borrow().clone()).is_err());
    }
}
