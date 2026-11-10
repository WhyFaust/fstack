use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, String>;
const MANIFEST: &str = ".codex-plugin/plugin.json";
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
fn check_versions(root: &Path) -> Result<String> {
    let data = manifest(root)?;
    let current = data["version"].as_str().ok_or("Нет версии")?;
    version(current)?;
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
    fn run(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>>;
    fn check(&self, root: &Path) -> Result<()> {
        crate::check(root).map(|_| ())
    }
}
struct System;
impl Runner for System {
    fn run(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new(args[0])
            .args(&args[1..])
            .current_dir(root)
            .output()
            .map_err(|e| e.to_string())?;
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
        self.runner.run(self.root, args)
    }
    fn cmd(&self, args: &[&str]) -> Result<String> {
        String::from_utf8(self.bytes(args)?)
            .map(|v| v.trim().to_owned())
            .map_err(|e| e.to_string())
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
    fn signature(&self, rev: &str, path: &str) -> Result<(serde_yaml::Value, String)> {
        let text = String::from_utf8(self.bytes(&["git", "show", &format!("{rev}:{path}")])?)
            .map_err(|e| e.to_string())?;
        let (mut data, body) = parse_skill(&text)?;
        data["metadata"]
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::String("version".into()));
        Ok((data, body))
    }
    fn changed(&self, base: &str) -> Result<Vec<String>> {
        let old = self.tree(base)?;
        let new = self.tree("HEAD")?;
        let names: BTreeSet<_> = new
            .keys()
            .filter(|p| rx(r"\Askills/[^/]+/SKILL\.md\z").is_match(p))
            .map(|p| p.split('/').nth(1).unwrap().to_owned())
            .collect();
        let mut changed = Vec::new();
        for name in names {
            let prefix = format!("skills/{name}/");
            let before: BTreeMap<_, _> =
                old.iter().filter(|(p, _)| p.starts_with(&prefix)).collect();
            let after: BTreeMap<_, _> =
                new.iter().filter(|(p, _)| p.starts_with(&prefix)).collect();
            let mut different =
                before.keys().collect::<Vec<_>>() != after.keys().collect::<Vec<_>>();
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
                        && self.signature(base, path)? == self.signature("HEAD", path)?
                    {
                        continue;
                    }
                    different = true;
                    break;
                }
            }
            if different {
                changed.push(name);
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
        let level = change_level(&self.cmd(&[
            "git",
            "log",
            "--format=%B%x00",
            &format!("{base}..HEAD"),
        ])?);
        let current = check_versions(self.root)?;
        let mut candidates = (0..=level)
            .map(|l| increment(&number, l))
            .collect::<Result<Vec<_>>>()?;
        let mut proposed = increment(&number, level)?;
        let head = self.cmd(&["git", "rev-parse", "HEAD"])?;
        if level > 0 {
            loop {
                let tag = format!("v{proposed}");
                let Some(record) = records.iter().find(|r| r["tag_name"] == tag) else {
                    break;
                };
                match self.draft_source(record)? {
                    Some(source) if source != head => {
                        proposed = increment(&proposed, 1)?;
                        candidates.push(proposed.clone());
                    }
                    Some(_) => break,
                    None => return Err(format!("Версия {tag} занята неподтверждённым релизом")),
                }
            }
        }
        if !candidates.contains(&current) {
            return Err(format!(
                "Версия {current} не соответствует опубликованной или ожидающей версии"
            ));
        }
        Ok(
            json!({"version":proposed,"level":level,"base":base,"skills":self.changed(&base)?,"records":records}),
        )
    }
    fn draft_source(&self, record: &Value) -> Result<Option<String>> {
        if record["draft"] != true || record["author"]["login"] != "github-actions[bot]" {
            return Ok(None);
        }
        let marker = rx(r"(?m)^<!-- release-source:([a-f0-9]{40}) -->$");
        let body = record["body"].as_str().unwrap_or("");
        let captures: Vec<_> = marker.captures_iter(body).collect();
        if captures.is_empty() {
            return Ok(None);
        }
        if captures.len() != 1 {
            return Err("Неоднозначная исходная ревизия черновика".into());
        }
        let sha = &captures[0][1];
        let tag = record["tag_name"].as_str().ok_or("Нет тега")?;
        let number = tag.strip_prefix('v').ok_or("Некорректный тег черновика")?;
        version(number)?;
        self.cmd(&["git", "merge-base", "--is-ancestor", sha, "HEAD"])?;
        let saved: Value =
            serde_json::from_str(&self.cmd(&["git", "show", &format!("{sha}:{MANIFEST}")])?)
                .map_err(|e| e.to_string())?;
        if saved["version"] != number {
            return Err("Версия черновика не совпадает с манифестом его коммита".into());
        }
        if !self.cmd(&["git", "tag", "--list", tag])?.is_empty()
            && self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])? != sha
        {
            return Err(format!("Тег {tag} указывает на другую ревизию"));
        }
        Ok(Some(sha.into()))
    }
    fn version_contents(&self, number: &str) -> Result<Option<Vec<u8>>> {
        version(number)?;
        let mut data = manifest(self.root)?;
        if data["version"] == number {
            return Ok(None);
        }
        data["version"] = json!(number);
        Ok(Some(
            (serde_json::to_string_pretty(&data).map_err(|e| e.to_string())? + "\n").into_bytes(),
        ))
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
                let parents = self.cmd(&["git", "rev-list", "--parents", "-n", "1", sha])?;
                if parents.split_whitespace().skip(1).collect::<Vec<_>>() != vec![source]
                    || self.cmd(&["git", "diff", "--name-only", source, sha])? != MANIFEST
                    || !self
                        .cmd(&["git", "diff", "--summary", source, sha])?
                        .is_empty()
                {
                    return Err(
                        "Исходная ревизия бинарного артефакта не совпадает с релизом".into(),
                    );
                }
                let mut before: Value = serde_json::from_slice(&self.bytes(&[
                    "git",
                    "show",
                    &format!("{source}:{MANIFEST}"),
                ])?)
                .map_err(|e| e.to_string())?;
                let after_bytes = self.bytes(&["git", "show", &format!("{sha}:{MANIFEST}")])?;
                let after: Value =
                    serde_json::from_slice(&after_bytes).map_err(|e| e.to_string())?;
                version(after["version"].as_str().ok_or("Нет версии артефакта")?)?;
                before["version"] = after["version"].clone();
                if (serde_json::to_string_pretty(&before).map_err(|e| e.to_string())? + "\n")
                    .as_bytes()
                    != after_bytes
                {
                    return Err("Коммит релиза изменяет не только версию манифеста".into());
                }
            }
            let binary = if target.contains("windows") {
                "fstack.exe"
            } else {
                "fstack"
            };
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
    fn archive(&self, number: &str, sha: &str) -> Result<Vec<PathBuf>> {
        use std::io::Write;
        let artifacts = self.artifacts(sha)?;
        let data = manifest(self.root)?;
        let name = data["name"].as_str().ok_or("Нет имени пакета")?;
        if !rx(r"\A[a-zA-Z0-9_-]+\z").is_match(name) {
            return Err("Некорректное имя пакета".into());
        }
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
            let binary = if target.contains("windows") {
                "fstack.exe"
            } else {
                "fstack"
            };
            archive
                .start_file(
                    format!("{name}/bin/{binary}"),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated)
                        .unix_permissions(0o755),
                )
                .map_err(|e| e.to_string())?;
            archive.write_all(&bytes).map_err(|e| e.to_string())?;
            archive.finish().map_err(|e| e.to_string())?;
            let checksum = output.with_extension("zip.sha256");
            let hash = format!(
                "{:x}",
                Sha256::digest(fs::read(&output).map_err(|e| e.to_string())?)
            );
            fs::write(
                &checksum,
                format!(
                    "{hash}  {}\n",
                    output.file_name().unwrap().to_string_lossy()
                ),
            )
            .map_err(|e| e.to_string())?;
            outputs.extend([output, checksum]);
        }
        Ok(outputs)
    }
    fn finish(
        &self,
        repo: &str,
        number: &str,
        records: &[Value],
        base: &str,
        sha: &str,
    ) -> Result<()> {
        version(number)?;
        let tag = format!("v{number}");
        let marker = format!("<!-- release-source:{sha} -->");
        let existing = records.iter().find(|r| r["tag_name"] == tag);
        if !self.cmd(&["git", "tag", "--list", &tag])?.is_empty()
            && self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])? != sha
        {
            return Err(format!("Тег {tag} указывает на другую ревизию"));
        }
        if let Some(record) = existing {
            if record["draft"] != true {
                return Err(format!(
                    "Релиз {tag} уже опубликован; проверьте его ревизию"
                ));
            }
            if record["author"]["login"] != "github-actions[bot]"
                || !record["body"].as_str().unwrap_or("").contains(&marker)
            {
                return Err(format!("Черновик {tag} не принадлежит этому выпуску"));
            }
        }
        let outputs = self.archive(number, sha)?;
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
        self.cmd(&[
            "gh",
            "release",
            "edit",
            &tag,
            "--repo",
            repo,
            "--draft=false",
            "--latest",
        ])?;
        println!("{}", json!({"published":tag,"sha":sha}));
        Ok(())
    }
    fn recover(&self, repo: &str, expected: &str, candidate: &str) -> Result<()> {
        let error="main изменился после начала проверки; дождитесь нового workflow или запустите workflow_dispatch для актуальной main";
        if !rx(r"\A[a-f0-9]{40}\z").is_match(candidate) {
            return Err(error.into());
        }
        self.cmd(&["git", "fetch", "--no-tags", "origin", candidate])?;
        let parents = self.cmd(&["git", "rev-list", "--parents", "-n", "1", candidate])?;
        if parents.split_whitespace().skip(1).collect::<Vec<_>>() != vec![expected] {
            return Err(error.into());
        }
        let subject = self.cmd(&["git", "log", "-1", "--format=%s", candidate])?;
        let pattern = rx(r"\Achore\(release\): (v[0-9]+\.[0-9]+\.[0-9]+)\z");
        let captured = pattern.captures(&subject).ok_or(error)?;
        let tag = &captured[1];
        let records = self.records(repo)?;
        if records.iter().filter(|r| r["tag_name"] == tag).any(|r| {
            r["author"]["login"] != "github-actions[bot]"
                || !r["body"]
                    .as_str()
                    .unwrap_or("")
                    .contains(&format!("<!-- release-source:{candidate} -->"))
        }) {
            return Err(error.into());
        }
        let plan = self.plan(
            records
                .into_iter()
                .filter(|r| r["tag_name"] != tag)
                .collect(),
        )?;
        let number = plan["version"].as_str().ok_or(error)?;
        if plan["level"] == 0 || subject != format!("chore(release): v{number}") {
            return Err(error.into());
        }
        let contents = self.version_contents(number)?;
        let changed = self.cmd(&["git", "diff", "--name-only", expected, candidate])?;
        if changed.lines().collect::<Vec<_>>()
            != if contents.is_some() {
                vec![MANIFEST]
            } else {
                vec![]
            }
            || !self
                .cmd(&["git", "diff", "--summary", expected, candidate])?
                .is_empty()
        {
            return Err(error.into());
        }
        if let Some(bytes) = contents {
            if self.bytes(&["git", "show", &format!("{candidate}:{MANIFEST}")])? != bytes {
                return Err(error.into());
            }
        }
        self.cmd(&["git", "checkout", "--detach", candidate])?;
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
        let remote = self.cmd(&[
            "git",
            "ls-remote",
            "--exit-code",
            "origin",
            "refs/heads/main",
        ])?;
        let live = remote
            .split_whitespace()
            .next()
            .ok_or("Не найдена удалённая main")?;
        self.artifacts(expected)?;
        if live != expected {
            self.recover(repo, expected, live)?;
        }
        let head = self.cmd(&["git", "rev-parse", "HEAD"])?;
        for record in self.records(repo)? {
            let Some(sha) = self.draft_source(&record)? else {
                continue;
            };
            let tag = record["tag_name"].as_str().ok_or("Нет тега")?;
            if sha != head {
                println!(
                    "{}",
                    json!({"status":"draft_preserved","tag":tag,"sha":sha,"message":"Старый черновик оставлен без изменений: текущие бинарные артефакты собраны из другой ревизии; его версия зарезервирована."})
                );
                continue;
            }
            let number = tag.strip_prefix('v').ok_or("Некорректный тег черновика")?;
            self.finish(
                repo,
                number,
                std::slice::from_ref(&record),
                &format!("{sha}^"),
                &sha,
            )?;
            self.cmd(&["git", "fetch", "origin", "tag", tag])?;
        }
        let plan = self.plan(self.records(repo)?)?;
        if plan["level"] == 0 {
            println!("Нет изменений, требующих релиза.");
            return Ok(());
        }
        let number = plan["version"].as_str().unwrap();
        let changed = self.version_contents(number)?;
        if let Some(content) = &changed {
            fs::write(self.root.join(MANIFEST), content).map_err(|e| e.to_string())?;
        }
        self.runner.check(self.root)?;
        self.cmd(&["git", "config", "user.name", "github-actions[bot]"])?;
        self.cmd(&[
            "git",
            "config",
            "user.email",
            "41898282+github-actions[bot]@users.noreply.github.com",
        ])?;
        if changed.is_some() {
            self.cmd(&["git", "add", "--", MANIFEST])?;
        }
        if !self
            .cmd(&["git", "diff", "--cached", "--name-only"])?
            .is_empty()
        {
            self.cmd(&["git", "commit", "-m", &format!("chore(release): v{number}")])?;
        }
        self.cmd(&["git", "push", "origin", "HEAD:main"])?;
        let sha = self.cmd(&["git", "rev-parse", "HEAD"])?;
        self.finish(
            repo,
            number,
            plan["records"].as_array().unwrap(),
            plan["base"].as_str().unwrap(),
            &sha,
        )
    }
}
const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];
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
    let name = if target.contains("windows") {
        "fstack.exe"
    } else {
        "fstack"
    };
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
    let artifacts = if action == "publish" && args.len() == 3 && args[1] == "--artifacts" {
        Some(Path::new(&args[2]))
    } else if action == "plan" && args.len() == 1 {
        None
    } else {
        return Err("Использование: fstack release check|plan|publish --artifacts DIR|artifact --target TARGET --binary PATH --output DIR".into());
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
    use std::cell::RefCell;
    struct Stub {
        records: RefCell<Vec<Value>>,
        calls: RefCell<Vec<Vec<String>>>,
        publish_tag: bool,
    }
    impl Runner for Stub {
        fn check(&self, root: &Path) -> Result<()> {
            check_versions(root).map(|_| ())
        }
        fn run(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>> {
            if args[0] != "gh" {
                return System.run(root, args);
            }
            if args[1] == "api" {
                return serde_json::to_vec(&vec![self.records.borrow().clone()])
                    .map_err(|e| e.to_string());
            }
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            if self.publish_tag && args[1..3] == ["release", "edit"] {
                System.run(root, &["git", "tag", args[3]])?;
                System.run(root, &["git", "push", "origin", args[3]])?;
                for record in self.records.borrow_mut().iter_mut() {
                    if record["tag_name"] == args[3] {
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
        }
    }
    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = vec!["git", "-c", "commit.gpgsign=false"];
        command.extend(args);
        String::from_utf8(System.run(root, &command).unwrap())
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
        assert_eq!(release.changed("v0.2.0").unwrap(), Vec::<String>::new());
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
        commit(&root, "feat: resources");
        assert_eq!(
            release.changed("v0.2.0").unwrap(),
            ["alpha", "beta", "gamma"]
        );
        fs::remove_file(root.join("skills/alpha/references/run.md")).unwrap();
        commit(&root, "refactor: undo resource");
        assert_eq!(release.changed("v0.2.0").unwrap(), ["beta", "gamma"]);
        let plan = release.plan(stub.records.borrow().clone()).unwrap();
        assert_eq!(plan["version"], "0.3.0");
        let skill_before = fs::read(&path).unwrap();
        save(
            &root,
            MANIFEST,
            release.version_contents("0.3.0").unwrap().unwrap(),
        );
        assert_eq!(fs::read(&path).unwrap(), skill_before);
        assert!(release.version_contents("0.3.0").unwrap().is_none());
        commit(&root, "chore(release): v0.3.0");
        assert_eq!(
            release.plan(stub.records.borrow().clone()).unwrap()["version"],
            "0.3.0"
        );
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
        let artifacts = artifacts(&root, &git(&root, &["rev-parse", "HEAD"]));
        let release = Release {
            root: &root,
            runner: &stub,
            artifacts: Some(&artifacts),
        };
        save(
            &root,
            MANIFEST,
            release.version_contents("0.2.1").unwrap().unwrap(),
        );
        commit(&root, "chore(release): v0.2.1");
        let sha = git(&root, &["rev-parse", "HEAD"]);
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
    fn rerun_recovers_only_exact_version_commit_without_double_publication() {
        for tampered in [false, true] {
            let (_temp, root) = fixture();
            let mut stub = stub();
            stub.publish_tag = true;
            save(&root, "skills/alpha/reference.md", "Validated instruction");
            commit(&root, "fix: validated change");
            let source = git(&root, &["rev-parse", "HEAD"]);
            let artifacts = artifacts(&root, &source);
            let release = Release {
                root: &root,
                runner: &stub,
                artifacts: Some(&artifacts),
            };
            save(
                &root,
                MANIFEST,
                release.version_contents("0.2.1").unwrap().unwrap(),
            );
            if tampered {
                save(&root, "skills/alpha/unvalidated.md", "Unvalidated");
            }
            commit(&root, "chore(release): v0.2.1");
            let candidate = git(&root, &["rev-parse", "HEAD"]);
            origin(&root);
            git(&root, &["checkout", "--detach", &source]);
            stub.records.borrow_mut().push(json!({"tag_name":"v0.2.1","draft":true,"prerelease":false,"author":{"login":"github-actions[bot]"},"body":format!("<!-- release-source:{candidate} -->")}));
            if tampered {
                assert!(release
                    .publish("fixture/fstack", &event(&source))
                    .unwrap_err()
                    .contains("main изменился"));
                assert_eq!(git(&root, &["rev-parse", "HEAD"]), source);
                assert!(stub.calls.borrow().is_empty());
                assert!(!root.join("dist").exists());
            } else {
                release.publish("fixture/fstack", &event(&source)).unwrap();
                assert_eq!(git(&root, &["rev-parse", "HEAD"]), candidate);
                assert_eq!(
                    stub.calls
                        .borrow()
                        .iter()
                        .map(|v| v[2].as_str())
                        .collect::<Vec<_>>(),
                    ["upload", "edit"]
                );
                stub.calls.borrow_mut().clear();
                git(&root, &["checkout", "--detach", &source]);
                release.publish("fixture/fstack", &event(&source)).unwrap();
                assert!(stub.calls.borrow().is_empty());
            }
        }
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
        release.publish("fixture/fstack", &event(&sha)).unwrap();
        assert_eq!(
            stub.calls
                .borrow()
                .iter()
                .map(|v| v[2].as_str())
                .collect::<Vec<_>>(),
            ["create", "upload", "edit"]
        );
        assert_eq!(git(&root, &["diff", "--name-only", &sha, "HEAD"]), MANIFEST);
        assert_eq!(check_versions(&root).unwrap(), "0.3.0");
        for target in TARGETS {
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
        }
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
    fn stale_owned_drafts_reserve_versions_without_blocking_current_release() {
        for message in ["docs: later clarification", "fix: later correction"] {
            let (_temp, root) = fixture();
            let stub = stub();
            save(&root, "validated.md", "Validated change");
            commit(&root, "fix: validated change");
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
            let stale = git(&root, &["rev-parse", "HEAD"]);
            git(&root, &["tag", "v0.2.1"]);
            let draft = json!({"tag_name":"v0.2.1","draft":true,"prerelease":false,"author":{"login":"github-actions[bot]"},"body":format!("<!-- release-source:{stale} -->")});
            stub.records.borrow_mut().push(draft.clone());
            save(&root, "later.md", "Later work");
            commit(&root, message);
            let source = git(&root, &["rev-parse", "HEAD"]);
            origin(&root);
            let artifacts = artifacts(&root, &source);
            let release = Release {
                root: &root,
                runner: &stub,
                artifacts: Some(&artifacts),
            };
            for invalid in [
                {
                    let mut record = draft.clone();
                    record["author"]["login"] = json!("human");
                    record
                },
                {
                    let mut record = draft.clone();
                    record["body"] = json!("No source marker");
                    record
                },
                {
                    let mut record = draft.clone();
                    record["body"] = json!(format!("<!-- release-source:{source} -->"));
                    record
                },
            ] {
                stub.records.borrow_mut()[1] = invalid;
                assert!(release.publish("fixture/fstack", &event(&source)).is_err());
                assert!(stub.calls.borrow().is_empty());
                assert_eq!(git(&root, &["rev-parse", "HEAD"]), source);
                assert_eq!(check_versions(&root).unwrap(), "0.2.1");
            }
            stub.records.borrow_mut()[1] = draft.clone();
            assert_eq!(
                release.plan(stub.records.borrow().clone()).unwrap()["version"],
                "0.2.2"
            );
            release.publish("fixture/fstack", &event(&source)).unwrap();
            assert_eq!(check_versions(&root).unwrap(), "0.2.2");
            assert_eq!(stub.records.borrow()[1], draft);
            assert_eq!(git(&root, &["rev-parse", "v0.2.1"]), stale);
            let calls = stub.calls.borrow();
            assert_eq!(
                calls.iter().map(|v| v[2].as_str()).collect::<Vec<_>>(),
                ["create", "upload", "edit"]
            );
            assert!(calls.iter().all(|v| v[3] == "v0.2.2"));
            assert!(!root
                .join("dist/fstack-0.2.1-x86_64-unknown-linux-gnu.zip")
                .exists());
        }
    }
}
