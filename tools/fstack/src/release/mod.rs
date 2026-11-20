mod assets;

use crate::{
    package::{self, PACKAGE_VERSION},
    version::parse as version,
};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, String>;
const MANIFEST: &str = ".codex-plugin/plugin.json";
const RUNTIME_ID: &str = "launchers/runtime-id";
const RELEASE_BY_TAG: &str = "query($owner:String!,$name:String!,$tag:String!){
  repository(owner:$owner,name:$name){release(tagName:$tag){
    node_id:id id:databaseId tag_name:tagName draft:isDraft prerelease:isPrerelease body:description author{login}
  }}
}";
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
struct PullRelease<'a> {
    number: u64,
    head: &'a str,
    base: &'a str,
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
    fn pull_at(&self, repo: &str, pull: &PullRelease<'_>) -> Result<Value> {
        self.check_live_main(pull.base)?;
        let promotion_ref = format!("refs/heads/release-candidates/pr-{}", pull.number);
        let remote = self.cmd(&["git", "ls-remote", "--exit-code", "origin", &promotion_ref])?;
        if remote.split_whitespace().next() != Some(pull.head) {
            return Err("Ветка продвижения PR изменилась".into());
        }
        let data: Value = serde_json::from_slice(&self.bytes(&[
            "gh",
            "api",
            &format!("repos/{repo}/pulls/{}", pull.number),
        ])?)
        .map_err(|e| e.to_string())?;
        if data["number"].as_u64() != Some(pull.number)
            || data["state"] != "open"
            || data["draft"] != false
            || !data["base"]["repo"]["full_name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(repo))
            || data["base"]["ref"] != "main"
            || data["base"]["sha"] != pull.base
            || data["head"]["sha"] != pull.head
        {
            return Err("Ревизии или состояние PR изменились перед публикацией".into());
        }
        Ok(data)
    }
    fn check_source(&self, repo: &str, head: &str, pull: Option<&PullRelease<'_>>) -> Result<()> {
        if let Some(pull) = pull {
            self.pull_at(repo, pull).map(|_| ())
        } else {
            self.check_live_main(head)
        }
    }
    fn records(&self, repo: &str) -> Result<Vec<Value>> {
        let (owner, name) = repo.split_once('/').ok_or("Некорректное имя репозитория")?;
        let tag = format!("v{}", check_versions(self.root)?);
        let lookup: Value = serde_json::from_str(&self.cmd(&[
            "gh",
            "api",
            "graphql",
            "-f",
            &format!("query={RELEASE_BY_TAG}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-f",
            &format!("tag={tag}"),
        ])?)
        .map_err(|e| e.to_string())?;
        let exact = lookup
            .pointer("/data/repository/release")
            .ok_or("GitHub не подтвердил поиск релиза по тегу")?;
        let pages: Vec<Vec<Value>> = serde_json::from_str(&self.cmd(&[
            "gh",
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/releases?per_page=100"),
        ])?)
        .map_err(|e| e.to_string())?;
        let mut records: Vec<_> = pages
            .into_iter()
            .flatten()
            .filter(|record| record["tag_name"] == tag)
            .collect();
        if !exact.is_null() {
            let id = exact["node_id"]
                .as_str()
                .ok_or("GitHub не вернул ID релиза")?;
            if exact["tag_name"] != tag {
                return Err("GitHub вернул релиз для другого тега".into());
            }
            if !records.iter().any(|record| record["node_id"] == id) {
                records.push(exact.clone());
            }
        }
        Ok(records)
    }
    fn same_package(&self, base: &str, head: &str) -> Result<bool> {
        let data = manifest(self.root)?;
        if self.manifest_at(base)? != data || !self.same_build_inputs(base, head)? {
            return Ok(false);
        }
        let mut paths = vec![
            "skills",
            "docs/skills.md",
            ".agents/plugins/marketplace.json",
            "assets",
            "licenses",
            "LICENSE",
        ];
        paths.extend(package::manifest_assets(&data)?);
        let tree = |rev| {
            let mut args = vec![
                "git",
                "--literal-pathspecs",
                "ls-tree",
                "-r",
                "-z",
                rev,
                "--",
            ];
            args.extend_from_slice(&paths);
            self.bytes(&args)
        };
        Ok(tree(base)? == tree(head)?)
    }
    fn tree(&self, revision: &str) -> Result<String> {
        self.cmd(&[
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{tree}}"),
        ])
    }
    fn has_pull_source(&self, record: &Value, sha: &str) -> Result<bool> {
        let footer = format!(
            "<!-- release-tree:{} -->\n<!-- release-source:{sha} -->",
            self.tree(sha)?
        );
        Ok(record["author"]["login"] == "github-actions[bot]"
            && record["body"]
                .as_str()
                .is_some_and(|s| s.trim_end().ends_with(&footer)))
    }
    fn contains_release(&self, tag: &str, head: &str, record: &Value) -> Result<bool> {
        let tag_sha = self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])?;
        if !self.has_pull_source(record, &tag_sha)? {
            return Ok(self.cmd(&["git", "merge-base", &tag_sha, head])? == tag_sha);
        }
        let tree = self.tree(tag)?;
        Ok(self
            .cmd(&["git", "log", "--first-parent", "--format=%T", head])?
            .lines()
            .any(|candidate| candidate == tree))
    }
    fn check_remote_tag(&self, tag: &str, sha: &str) -> Result<()> {
        let reference = format!("refs/tags/{tag}");
        let peeled = format!("{reference}^{{}}");
        let remote = self.cmd(&[
            "git",
            "ls-remote",
            "--exit-code",
            "origin",
            &reference,
            &peeled,
        ])?;
        let refs: BTreeMap<_, _> = remote
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(sha, name)| (name, sha))
            .collect();
        if refs
            .get(peeled.as_str())
            .or_else(|| refs.get(reference.as_str()))
            .copied()
            != Some(sha)
        {
            return Err(format!("Удалённый тег {tag} указывает на другую ревизию"));
        }
        Ok(())
    }
    fn plan(&self, records: &[Value]) -> Result<Value> {
        let number = check_versions(self.root)?;
        let tag = format!("v{number}");
        self.check_runtime_id("HEAD")?;
        let mut matches = records.iter().filter(|record| record["tag_name"] == tag);
        let record = matches.next();
        if matches.next().is_some() {
            return Err(format!("Найдено несколько выпусков {tag}"));
        }
        if let Some(record) = record {
            if record["draft"] == true {
                self.draft_target(record, "HEAD", &number)?;
            } else {
                if self.cmd(&["git", "tag", "--list", &tag])?.is_empty() {
                    return Err(format!("GitHub Release {tag} уже опубликован, но Git-тег отсутствует. Проверьте теги или выберите свободную версию."));
                }
                if record["prerelease"] != false
                    || !self.contains_release(&tag, "HEAD", record)?
                    || !self.same_package(&tag, "HEAD")?
                {
                    return Err(format!(
                        "Версия {number} уже опубликована с другими исходниками"
                    ));
                }
                return Ok(json!({"version":number,"tag":tag,"publish":false}));
            }
        }
        if record.is_none() && !self.cmd(&["git", "tag", "--list", &tag])?.is_empty() {
            return Err(format!(
                "Версия {number} уже занята тегом без соответствующего выпуска"
            ));
        }
        Ok(json!({"version":number,"tag":tag,"publish":true}))
    }
    fn check_next(&self, base: &str) -> Result<String> {
        let base = self.cmd(&[
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ])?;
        self.cmd(&["git", "merge-base", "--is-ancestor", &base, "HEAD"])?;
        let current = check_versions(self.root)?;
        self.check_runtime_id("HEAD")?;
        let previous = self.manifest_at(&base)?;
        let before = previous["version"]
            .as_str()
            .ok_or("Нет версии базового пакета")?;
        let old_version = version(before)?;
        let new_version = version(&current)?;
        if before == current {
            if !self.same_package(&base, "HEAD")? {
                return Err("Изменение пакета требует новой версии".into());
            }
        } else if old_version[0] == new_version[0] && new_version < old_version {
            return Err("Уменьшение версии в пределах серии недопустимо".into());
        } else if !self
            .cmd(&["git", "tag", "--list", &format!("v{current}")])?
            .is_empty()
            && self.tree(&format!("v{current}"))? != self.tree("HEAD")?
        {
            return Err(format!("Версия {current} уже занята"));
        }
        Ok(current)
    }
    fn prepare(&self, number: Option<&str>) -> Result<String> {
        let data = manifest(self.root)?;
        let number = number
            .or_else(|| data["version"].as_str())
            .ok_or("Нет версии пакета")?;
        package::check_package_major(self.root, number)?;
        let tree = self.cmd(&["git", "write-tree"])?;
        let runtime_id = self.runtime_id(&tree)?;
        if let Some(bytes) = updated_manifest(data.clone(), number)? {
            fs::write(self.root.join(MANIFEST), bytes).map_err(|e| e.to_string())?;
        }
        fs::write(self.root.join(PACKAGE_VERSION), format!("{number}\n"))
            .map_err(|e| e.to_string())?;
        fs::write(self.root.join(RUNTIME_ID), format!("{runtime_id}\n"))
            .map_err(|e| e.to_string())?;
        Ok(number.into())
    }
    fn manifest_at(&self, revision: &str) -> Result<Value> {
        serde_json::from_slice(&self.bytes(&["git", "show", &format!("{revision}:{MANIFEST}")])?)
            .map_err(|e| e.to_string())
    }
    fn archive(
        &self,
        number: &str,
        sha: &str,
        artifacts: &[(String, Vec<u8>)],
        directory: &Path,
    ) -> Result<Vec<PathBuf>> {
        let data = manifest(self.root)?;
        let name = data["name"].as_str().ok_or("Нет имени пакета")?;
        if !rx(r"\A[a-zA-Z0-9_-]+\z").is_match(name) {
            return Err("Некорректное имя пакета".into());
        }
        let runtime_id = self.check_runtime_id(sha)?;
        let mut outputs = Vec::new();
        for (target, bytes) in artifacts {
            let output = directory.join(format!("{name}-{number}-{target}.zip"));
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
        use pulldown_cmark::{Event, Parser};

        let body = record["body"].as_str().unwrap_or("").trim_end();
        let marker = rx(r"\A<!-- release-source:([a-f0-9]{40}) -->\z");
        let source = |line: &str| {
            marker
                .captures(line.trim_end())
                .map(|capture| capture[1].to_owned())
        };
        let (notes, last_line) = body.rsplit_once('\n').unwrap_or(("", body));
        let footer = source(last_line);
        let mut matches = Parser::new(if footer.is_some() { notes } else { body }).filter_map(
            |event| match event {
                Event::Html(html) => source(&html),
                _ => None,
            },
        );
        let target = footer
            .or_else(|| matches.next())
            .ok_or("Нет ревизии черновика")?;
        if matches.next().is_some()
            || record["author"]["login"] != "github-actions[bot]"
            || record["draft"] != true
        {
            return Err("Черновик не принадлежит этому выпуску".into());
        }
        self.cmd(&["git", "merge-base", "--is-ancestor", &target, head])?;
        if self.manifest_at(&target)?["version"] != number || !self.same_package(&target, head)? {
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
    fn release_notes(&self, repo: &str, number: &str, sha: &str) -> Result<String> {
        let version_commit = self.cmd(&[
            "git",
            "log",
            "--first-parent",
            "-1",
            "--format=%H",
            sha,
            "--",
            PACKAGE_VERSION,
        ])?;
        let pages: Vec<Vec<Value>> = serde_json::from_str(&self.cmd(&[
            "gh",
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/commits/{version_commit}/pulls?per_page=100"),
        ])?)
        .map_err(|e| e.to_string())?;
        let mut sections = Vec::new();
        for pull in pages.into_iter().flatten().filter(|pull| {
            pull["merged_at"].is_string()
                && pull["base"]["ref"] == "main"
                && pull["base"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(repo))
        }) {
            if let Some(section) = pull["body"].as_str().and_then(upgrade_section) {
                if !sections.contains(&section) {
                    sections.push(section);
                }
            }
        }
        Ok(Self::notes(number, sha, &sections, None))
    }
    fn notes(number: &str, sha: &str, sections: &[String], tree: Option<&str>) -> String {
        let mut notes = format!("Пакет fstack {number}. Архивы, бинарники и SHA-256.");
        if !sections.is_empty() {
            notes.push_str("\n\n## При обновлении\n\n");
            notes.push_str(&sections.join("\n\n"));
        }
        notes.push_str("\n\n");
        if let Some(tree) = tree {
            notes.push_str(&format!("<!-- release-tree:{tree} -->\n"));
        }
        format!("{notes}<!-- release-source:{sha} -->")
    }
    fn finish(
        &self,
        repo: &str,
        number: &str,
        records: &[Value],
        head: &str,
        pull: Option<&PullRelease<'_>>,
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
        if pull.is_some() && sha != head {
            return Err("Черновик подготовлен для другой ревизии PR".into());
        }
        if !self.cmd(&["git", "tag", "--list", &tag])?.is_empty()
            && self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])? != sha
        {
            return Err(format!("Тег {tag} указывает на другую ревизию"));
        }
        self.check_source(repo, head, pull)?;
        let artifacts = self.artifacts(head)?;
        let temporary = pull
            .map(|_| tempfile::tempdir())
            .transpose()
            .map_err(|e| e.to_string())?;
        let dist = self.root.join("dist");
        let directory = temporary.as_ref().map_or(dist.as_path(), |dir| dir.path());
        let outputs = self.archive(number, sha, &artifacts, directory)?;
        if existing.is_none() {
            let notes = if let Some(pull) = pull {
                let data = self.pull_at(repo, pull)?;
                let section = data["body"].as_str().and_then(upgrade_section);
                Self::notes(number, sha, section.as_slice(), Some(&self.tree(sha)?))
            } else {
                self.release_notes(repo, number, sha)?
            };
            self.command(
                &[
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
                    "-",
                    "--draft",
                ],
                Some(notes.as_bytes()),
            )?;
        }
        let draft_id = if pull.is_some() {
            let drafts = self.records(repo)?;
            let [record] = drafts.as_slice() else {
                return Err("GitHub не подтвердил единственный черновик выпуска".into());
            };
            if self.draft_target(record, head, number)? != sha
                || !self.has_pull_source(record, sha)?
            {
                return Err("Черновик принадлежит другой ревизии PR".into());
            }
            Some(record["id"].as_u64().ok_or("Нет ID черновика выпуска")?)
        } else {
            None
        };
        let output_names: Vec<_> = outputs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut upload = vec!["gh", "release", "upload", &tag];
        upload.extend(output_names.iter().map(String::as_str));
        upload.extend(["--repo", repo, "--clobber"]);
        self.cmd(&upload)?;
        if let Some(id) = draft_id {
            let draft: Value = serde_json::from_slice(&self.bytes(&[
                "gh",
                "api",
                &format!("repos/{repo}/releases/{id}"),
            ])?)
            .map_err(|e| e.to_string())?;
            if draft["id"].as_u64() != Some(id)
                || draft["tag_name"] != tag
                || self.draft_target(&draft, head, number)? != sha
                || !self.has_pull_source(&draft, sha)?
            {
                return Err("Источник загруженного черновика не совпадает с ревизией PR".into());
            }
            self.verify_inventory(repo, number, sha, Some(&outputs), id)?;
        }
        self.check_source(repo, head, pull)?;
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
            if pull.is_some() {
                "--latest=false"
            } else {
                "--latest"
            },
        ])?;
        if pull.is_some() {
            self.check_remote_tag(&tag, sha)?;
            self.verify_assets(repo, number, sha, Some(&outputs))?;
        }
        println!("{}", json!({"published":tag,"sha":sha}));
        Ok(())
    }
    fn publish(&self, repo: &str, event: &BTreeMap<String, String>) -> Result<()> {
        self.publish_from(repo, event, None)
    }
    fn publish_pr(
        &self,
        repo: &str,
        event: &BTreeMap<String, String>,
        pull: &PullRelease<'_>,
    ) -> Result<()> {
        self.publish_from(repo, event, Some(pull))
    }
    fn publish_from(
        &self,
        repo: &str,
        event: &BTreeMap<String, String>,
        pull: Option<&PullRelease<'_>>,
    ) -> Result<()> {
        let branch = pull.map_or_else(
            || "main".to_owned(),
            |p| format!("release-candidates/pr-{}", p.number),
        );
        self.check_workflow(repo, event, &branch)?;
        let workflow_sha = event.get("GITHUB_SHA").map(String::as_str).unwrap_or("");
        let expected = if let Some(pull) = pull {
            if event.get("GITHUB_EVENT_NAME").map(String::as_str) != Some("push")
                || pull.number == 0
                || pull.head != workflow_sha
                || !rx(r"\A[a-f0-9]{40}\z").is_match(pull.base)
                || pull.base == pull.head
            {
                return Err(
                    "Для выпуска PR нужен push проверенного SHA в ветку продвижения".into(),
                );
            }
            pull.head
        } else {
            workflow_sha
        };
        if !rx(r"\A[a-f0-9]{40}\z").is_match(expected)
            || self.cmd(&["git", "rev-parse", "HEAD"])? != expected
        {
            return Err("Релиз должен начинаться с проверенного SHA события workflow".into());
        }
        if !self.cmd(&["git", "status", "--porcelain"])?.is_empty() {
            return Err("Рабочее дерево содержит изменения".into());
        }
        self.check_source(repo, expected, pull)?;
        if let Some(pull) = pull {
            let number = self.check_next(pull.base)?;
            if self.manifest_at(pull.base)?["version"] == number {
                return Err("Для выпуска PR нужна новая версия".into());
            }
        }
        let records = self.records(repo)?;
        let plan = self.plan(&records)?;
        if plan["publish"] != true {
            if pull.is_some() {
                let tag = plan["tag"].as_str().ok_or("Нет тега выпуска")?;
                let record = records
                    .iter()
                    .find(|r| r["tag_name"] == tag)
                    .ok_or("Нет записи выпуска")?;
                if self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])? != expected
                    || !self.has_pull_source(record, expected)?
                {
                    return Err("Версия уже опубликована для другой ревизии PR".into());
                }
                self.check_remote_tag(tag, expected)?;
                let number = plan["version"].as_str().ok_or("Нет версии выпуска")?;
                let artifacts = self.artifacts(expected)?;
                let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
                let outputs = self.archive(number, expected, &artifacts, directory.path())?;
                self.verify_assets(repo, number, expected, Some(&outputs))?;
            }
            println!("Текущая версия уже опубликована.");
            return Ok(());
        }
        self.runner.check(self.root)?;
        self.finish(
            repo,
            plan["version"].as_str().unwrap(),
            &records,
            expected,
            pull,
        )
    }
    fn check_workflow(
        &self,
        repo: &str,
        event: &BTreeMap<String, String>,
        branch: &str,
    ) -> Result<()> {
        if event.get("GITHUB_ACTIONS").map(String::as_str) != Some("true")
            || event.get("GITHUB_REF") != Some(&format!("refs/heads/{branch}"))
            || event.get("GITHUB_REPOSITORY").map(String::as_str) != Some(repo)
        {
            return Err(format!(
                "Публикация выполняется только workflow ветки {branch} в {repo}"
            ));
        }
        Ok(())
    }
    fn latest(&self, repo: &str) -> Result<Option<String>> {
        let (owner, name) = repo.split_once('/').ok_or("Некорректное имя репозитория")?;
        let data: Value = serde_json::from_slice(&self.bytes(&[
            "gh","api","graphql","-f","query=query($owner:String!,$name:String!){repository(owner:$owner,name:$name){latestRelease{tagName}}}",
            "-f",&format!("owner={owner}"),"-f",&format!("name={name}"),
        ])?).map_err(|e| e.to_string())?;
        let latest = data
            .pointer("/data/repository/latestRelease")
            .ok_or("GitHub не подтвердил последний выпуск")?;
        if latest.is_null() {
            return Ok(None);
        }
        latest["tagName"]
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or("Неверный тег последнего выпуска".into())
    }
    fn promote(&self, repo: &str, event: &BTreeMap<String, String>) -> Result<()> {
        self.check_workflow(repo, event, "main")?;
        let head = self.cmd(&["git", "rev-parse", "HEAD"])?;
        if event.get("GITHUB_SHA") != Some(&head)
            || !matches!(
                event.get("GITHUB_EVENT_NAME").map(String::as_str),
                Some("push" | "workflow_dispatch")
            )
            || !self.cmd(&["git", "status", "--porcelain"])?.is_empty()
        {
            return Err("Продвижение требует чистую проверенную ревизию main".into());
        }
        self.check_live_main(&head)?;
        let records = self.records(repo)?;
        let plan = self.plan(&records)?;
        if plan["publish"] != false {
            return Err("Нет опубликованного пакета для продвижения".into());
        }
        let number = check_versions(self.root)?;
        let tag = format!("v{number}");
        let source = self.cmd(&["git", "rev-parse", &format!("{tag}^{{commit}}")])?;
        self.check_remote_tag(&tag, &source)?;
        self.verify_assets(repo, &number, &source, None)?;
        if self.latest(repo)?.as_deref() != Some(&tag) {
            let record = records
                .iter()
                .find(|r| r["tag_name"] == tag)
                .ok_or("Нет записи выпуска")?;
            if !self.has_pull_source(record, &source)? {
                return Err("Нет проверенного источника выпуска PR".into());
            }
            self.check_live_main(&head)?;
            self.check_remote_tag(&tag, &source)?;
            self.cmd(&["gh", "release", "edit", &tag, "--repo", repo, "--latest"])?;
            if self.latest(repo)?.as_deref() != Some(&tag) {
                return Err("GitHub не подтвердил продвижение пакета".into());
            }
        }
        println!("{}", json!({"latest":tag,"sha":source}));
        Ok(())
    }
}
fn upgrade_section(body: &str) -> Option<String> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag};

    let mut depth = 0;
    let mut section = None;
    let mut end = body.len();
    for (event, span) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    if let Tag::Heading {
                        level: level @ (HeadingLevel::H1 | HeadingLevel::H2),
                        ..
                    } = tag
                    {
                        if section.is_some() {
                            end = span.start;
                            break;
                        }
                        let title: String = Parser::new(&body[span.clone()])
                            .filter_map(|event| match event {
                                Event::Text(text) | Event::Code(text) => Some(text.into_string()),
                                _ => None,
                            })
                            .collect();
                        if level == HeadingLevel::H2 && title.trim() == "При обновлении"
                        {
                            section = Some(span.end);
                        }
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => (),
        }
    }
    let section = &body[section?..end];
    let mut text = String::new();
    let mut cursor = 0;
    let comment = rx(r"(?s)<!--.*?(?:-->|\z)");
    for (event, span) in Parser::new(section).into_offset_iter() {
        if matches!(event, Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_)) {
            let html = &section[span.clone()];
            text.push_str(&section[cursor..span.start]);
            text.push_str(&comment.replace_all(html, ""));
            cursor = span.end;
        }
    }
    text.push_str(&section[cursor..]);
    let lines: Vec<_> = text.lines().collect();
    let first = lines.iter().position(|line| !line.trim().is_empty())?;
    let last = lines.iter().rposition(|line| !line.trim().is_empty())?;
    Some(lines[first..=last].join("\n"))
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
    let local = Release {
        root: package,
        runner: &System,
        artifacts: None,
    };
    if action == "prepare" && matches!(args.len(), 1 | 2) {
        let number = args
            .get(1)
            .map(|arg| arg.to_str().ok_or("Некорректная версия пакета"))
            .transpose()?;
        println!("{}", local.prepare(number)?);
        return Ok(());
    }
    if action == "check-next" && args.len() == 2 {
        let base = args[1].to_str().ok_or("Некорректная база PR")?;
        println!("{}", local.check_next(base)?);
        return Ok(());
    }
    let pull = if action == "publish-pr" && args.len() == 9 {
        let field = |name| {
            option(&args[1..], name)?
                .to_str()
                .ok_or("Некорректный параметр PR".to_owned())
        };
        Some(PullRelease {
            number: field("--pr")?
                .parse()
                .map_err(|_| "Некорректный номер PR")?,
            head: field("--head")?,
            base: field("--base")?,
        })
    } else {
        None
    };
    let artifacts = if pull.is_some()
        || (action == "publish" && args.len() == 3 && args[1] == "--artifacts")
    {
        Some(Path::new(option(&args[1..], "--artifacts")?))
    } else if matches!(action, "plan" | "promote") && args.len() == 1 {
        None
    } else {
        return Err("Использование: fstack release check|prepare [VERSION]|check-next BASE|plan|publish --artifacts DIR|publish-pr --pr N --head SHA --base SHA --artifacts DIR|promote|artifact --target TARGET --binary PATH --output DIR".into());
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
    if let Some(pull) = pull {
        release.publish_pr(&repo, &std::env::vars().collect(), &pull)
    } else if action == "promote" {
        release.promote(&repo, &std::env::vars().collect())
    } else if action == "publish" {
        release.publish(&repo, &std::env::vars().collect())
    } else {
        let plan = release.plan(&release.records(&repo)?)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?
        );
        Ok(())
    }
}
#[cfg(test)]
mod tests;
