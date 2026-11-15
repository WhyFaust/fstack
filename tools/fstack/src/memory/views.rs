use super::{format, io, Result};
use std::path::{Path, PathBuf};
pub const VAULT_FILES: &[&str] = &[
    "Home.md",
    "Memory.canvas",
    "Library.base",
    "Templates/Decision.md",
    "Templates/Correction.md",
    "Templates/Preference.md",
    "Templates/Review.md",
];
pub fn asset(package: &Path, name: &str) -> Result<String> {
    std::fs::read_to_string(package.join("skills/memory/assets/vault").join(name)).map_err(io::err)
}
pub fn marker(text: Option<&str>, marker: &str) -> bool {
    text.and_then(|s| s.strip_prefix("---\n"))
        .and_then(|s| s.split_once("\n---\n"))
        .is_some_and(|(meta, _)| meta.lines().any(|s| s == marker))
}
pub fn project_page(root: &Path) -> PathBuf {
    let name = root.file_name().unwrap().to_string_lossy();
    root.join(format!(
        "{}.md",
        if name == "project" { "Project" } else { &name }
    ))
}
pub fn project_marker(raw: &str, project: &str) -> bool {
    let Some((meta, _)) = raw
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n"))
    else {
        return false;
    };
    let values = meta
        .lines()
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| k.trim_end() == "fstack_memory_project")
        .map(|(_, v)| v.trim())
        .collect::<Vec<_>>();
    values.len() == 1
        && format::matches(
            &format!(r#"\A(?:{project}|"{project}"|'{project}')(?:[ \t]+#.*)?\z"#),
            values[0],
        )
}
fn has_marker(path: &Path, project: &str) -> Result<bool> {
    let Some(data) = io::prefix(path, 32768)? else {
        return Ok(false);
    };
    let re = regex::bytes::Regex::new(r"(?-u)\A(?:\xef\xbb\xbf)?---\r?\n(?:[^\n]*\n)*?---\r?\n")
        .unwrap();
    let Some(m) = re.find(&data) else {
        return Ok(false);
    };
    let text = std::str::from_utf8(m.as_bytes())
        .map_err(io::err)?
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    Ok(project_marker(&text, project))
}
pub fn current_page(root: &Path) -> Result<Option<PathBuf>> {
    let project = root.file_name().unwrap().to_string_lossy();
    let p = project_page(root);
    if has_marker(&p, &project)? {
        return Ok(Some(p));
    }
    let old = root.join("Project.md");
    if old != p && has_marker(&old, &project)? {
        return Ok(Some(old));
    }
    Ok(None)
}
pub fn project_link(page: &Path) -> String {
    let project = page
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    format!(
        "[[Codex Memory/Projects/{project}/{}|{project}]]",
        page.file_stem().unwrap().to_string_lossy()
    )
}
pub fn current_link(root: &Path) -> Result<String> {
    if super::shared::is_root(root) {
        return Ok(super::shared::LINK.into());
    }
    current_page(root)?
        .map(|p| project_link(&p))
        .ok_or("Memory project page is unavailable".into())
}
pub fn ensure_hub(root: &Path, package: &Path) -> Result<()> {
    let p = root.join("Projects.md");
    if io::read(&p)?.is_none() {
        io::write(&p, &asset(package, "Projects.md")?, false, None)?;
    }
    if !marker(io::read(&p)?.as_deref(), "fstack_memory_projects: 1") {
        return Err("Memory projects hub is occupied".into());
    }
    Ok(())
}
pub fn ensure_library(root: &Path, package: &Path) -> Result<()> {
    let destination = root.join("Library.base");
    if io::read(&destination)?.is_none() {
        io::write(&destination, &asset(package, "Library.base")?, false, None)?;
    }
    Ok(())
}
