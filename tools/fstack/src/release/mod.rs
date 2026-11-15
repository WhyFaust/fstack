use crate::{
    package::{self, PACKAGE_SERIES, PACKAGE_VERSION},
    version::{change_level, parse as version, series_version},
};

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
fn check_versions(root: &Path) -> Result<String> {
    let data = manifest(root)?;
    let number = data["version"].as_str().ok_or("Нет версии")?;
    package::check_versions(root, number)?;
    Ok(number.into())
}
trait Runner {
    fn run(&self, root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>>;
    fn check(&self, root: &Path) -> Result<()> {
        package::check(root).map(|_| ())
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
    fn renamed_skill(
        &self,
        head: &str,
        entry: &str,
        old: &BTreeMap<String, String>,
        new: &BTreeMap<String, String>,
        renamed: &mut BTreeSet<String>,
    ) -> Result<Option<String>> {
        let (data, _) = self.skill_at(head, entry)?;
        let Some(previous) = data["metadata"]["previous-name"].as_str() else {
            return Ok(None);
        };
        if !package::valid_skill_name(previous) {
            return Err(format!("{entry}: некорректное прежнее имя навыка"));
        }
        let path = format!("skills/{previous}/SKILL.md");
        if !old.contains_key(&path) || new.contains_key(&path) || !renamed.insert(path.clone()) {
            return Err(format!(
                "{entry}: невозможно подтвердить переименование {previous}"
            ));
        }
        Ok(Some(path))
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
        let mut renamed = BTreeSet::new();
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
                let previous_entry = if old.contains_key(&entry) {
                    Some(entry.clone())
                } else {
                    self.renamed_skill(head, &entry, &old, &new, &mut renamed)?
                };
                let before = previous_entry
                    .map(|path| self.skill_version(base, &path))
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
    fn published_release(&self, records: &[Value]) -> Result<Option<(String, String)>> {
        let tags: Vec<_> = records
            .iter()
            .filter(|r| r["draft"] == false && r["prerelease"] == false)
            .filter_map(|r| r["tag_name"].as_str())
            .filter(|tag| {
                tag.strip_prefix('v')
                    .is_some_and(|value| version(value).is_ok())
            })
            .collect();
        if tags.is_empty() {
            return Ok(None);
        }
        let mut args = vec!["git", "describe", "--first-parent", "--tags", "--abbrev=0"];
        for tag in tags {
            args.extend(["--match", tag]);
        }
        args.push("HEAD");
        let tag = self.cmd(&args)?;
        let number = tag.strip_prefix('v').ok_or("Некорректный тег выпуска")?;
        Ok(Some((tag.clone(), number.to_owned())))
    }
    fn plan(&self, records: Vec<Value>) -> Result<Value> {
        let (base, number) = if let Some(release) = self.published_release(&records)? {
            release
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
        let before = self.series_at(base)?;
        let after = self.series_at(head)?;
        series_version(previous, before.as_deref(), after.as_deref(), level)
    }
    fn series_at(&self, revision: &str) -> Result<Option<String>> {
        if self
            .cmd(&["git", "ls-tree", revision, "--", PACKAGE_SERIES])?
            .is_empty()
        {
            return Ok(None);
        }
        self.cmd(&["git", "show", &format!("{revision}:{PACKAGE_SERIES}")])
            .map(Some)
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
        let tag = format!("v{expected}");
        if level != 0 && !self.cmd(&["git", "tag", "--list", &tag])?.is_empty() {
            return Err(format!("Версия {expected} уже занята тегом {tag}"));
        }
        if write {
            let runtime_id = self.runtime_id(&inputs)?;
            fs::write(self.root.join(PACKAGE_VERSION), format!("{expected}\n"))
                .map_err(|e| e.to_string())?;
            fs::write(self.root.join(RUNTIME_ID), format!("{runtime_id}\n"))
                .map_err(|e| e.to_string())?;
        } else {
            self.check_runtime_id("HEAD")?;
            package::check_launcher_version(self.root, &expected)?;
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
mod tests;
