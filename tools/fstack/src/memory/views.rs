use super::{format, io, store, Result};
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
const LINK: &str = "[[Codex Memory/Projects|Все проекты]]";
const OLD_LINK: &str = "[[Codex Memory/Home|Все проекты]]";
const OLD_BASE:&str="```base\nfilters:\n  and:\n    - 'file.inFolder(\"Codex Memory/Projects\")'\n    - 'file.path.endsWith(\"/Project.md\")'\n    - 'note.fstack_memory_project != null'\nformulas:\n  project: 'link(file.path, note.fstack_memory_project)'\nviews:\n  - type: list\n    name: Проекты\n    order: [formula.project]\n```";
pub fn asset(package: &Path, name: &str) -> Result<String> {
    std::fs::read_to_string(package.join("skills/use-memory/assets/vault").join(name))
        .map_err(io::err)
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
    let p = root.join("Home.md");
    let raw = io::read(&p)?.ok_or("Memory home is unavailable")?;
    let updated = if raw.contains(OLD_BASE) {
        raw.replacen(OLD_BASE, LINK, 1)
    } else if !raw.contains(LINK) {
        format!("{raw}\n\n{LINK}\n")
    } else {
        return Ok(());
    };
    io::write(&p, &updated, true, Some(&raw))?;
    Ok(())
}
pub fn ensure_library(root: &Path, package: &Path) -> Result<()> {
    let p = root.join("Library.base");
    if io::read(&p)?.is_none() {
        io::write(&p, &asset(package, "Library.base")?, false, None)?;
    }
    let raw = io::read(&p)?.ok_or("Cannot initialize memory library")?;
    let approval = "file(file.path.replace(\"/Records/\", \"/Approved/\"))";
    let old = "file(file.path.replace(\"/Records/\", \"/Forgotten/\"))";
    let new = "file(file.folder + \"/\" + file.basename + \"-forgotten.json\")";
    let changes=[(format!("{approval} != null"),format!("(note.fstack_status == \"approved\" || {approval} != null)")),(format!("{approval} == null"),format!("(note.fstack_status != \"approved\" && {approval} == null)")),(format!("{old} == null"),format!("({new} == null && {old} == null)")),("!this.file.path.endsWith(\"/Project.md\") || file.inFolder(this.file.folder)".into(),"this.file.properties.fstack_memory_project == null || file.folder == \"Codex Memory/Projects/\" + this.file.properties.fstack_memory_project + \"/Records\"".into())];
    let mut updated = raw.clone();
    for (old, new) in changes {
        let r = regex::Regex::new(&format!(
            r"(?m)^([ \t]*(?:filters:|-)[ \t]+)('?){}('?)([ \t]*(?:#[^\n]*)?)$",
            regex::escape(&old)
        ))
        .unwrap();
        updated = r
            .replace_all(&updated, |c: &regex::Captures| {
                if c[2] != c[3] {
                    c[0].into()
                } else {
                    format!("{}{}{new}{}{}", &c[1], &c[2], &c[3], &c[4])
                }
            })
            .into_owned();
    }
    if updated != raw {
        io::write(&p, &updated, true, Some(&raw))?;
    }
    Ok(())
}
fn named(raw: &str) -> Result<String> {
    let (meta, body) = raw
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n"))
        .ok_or("Invalid project page")?;
    let navigation = format!("{OLD_LINK} · [[Codex Memory/Templates/Review|Шаблон обзора]]");
    let r = regex::Regex::new(&format!("(?m)^{}$", regex::escape(&navigation))).unwrap();
    let body = if r.is_match(body) {
        r.replacen(body, 1, navigation.replace(OLD_LINK, LINK))
            .into_owned()
    } else if !body.contains(LINK) {
        format!("\n{LINK}\n{body}")
    } else {
        body.into()
    };
    Ok(format!("---\n{meta}\n---\n{body}"))
}
pub fn migrate_project(root: &Path) -> Result<(usize, usize)> {
    let old = root.join("Project.md");
    let page = project_page(root);
    let Some(raw) = io::read(&old)? else {
        return Ok((
            0,
            usize::from(!has_marker(
                &page,
                &root.file_name().unwrap().to_string_lossy(),
            )?),
        ));
    };
    if !project_marker(&raw, &root.file_name().unwrap().to_string_lossy()) {
        return Ok((0, 1));
    }
    let desired = named(&raw)?;
    if desired.len() > 32768 {
        return Err("Updated memory project page is too large".into());
    }
    if old == page {
        return Ok((
            usize::from(desired != raw && io::write(&page, &desired, true, Some(&raw))?),
            0,
        ));
    }
    if io::write(&page, &desired, false, None)? {
        Ok((1, 0))
    } else {
        Ok((
            0,
            usize::from(io::read(&page)?.as_deref() != Some(&desired)),
        ))
    }
}
pub fn prune_project(root: &Path) -> Result<(usize, usize)> {
    let old = root.join("Project.md");
    let page = project_page(root);
    if old == page {
        return Ok((0, 0));
    }
    let Some(raw) = io::read(&old)? else {
        return Ok((0, 0));
    };
    if !project_marker(&raw, &root.file_name().unwrap().to_string_lossy())
        || !marker(
            io::read(&root.parent().unwrap().parent().unwrap().join("Projects.md"))?.as_deref(),
            "fstack_memory_projects: 1",
        )
    {
        return Ok((0, 1));
    }
    let desired = named(&raw)?;
    if io::read(&page)?.as_deref() != Some(&desired)
        || !store::files(&root.join("Approved"), false)?.is_empty()
    {
        return Ok((0, 1));
    }
    let expected = project_link(&page);
    let short = format!("{}]]", expected.split('|').next().unwrap());
    for p in store::files(&root.join("Records"), false)? {
        match store::note(
            &p,
            &p.file_stem().unwrap().to_string_lossy(),
            format::MEMORY,
            true,
        ) {
            Ok(Some(f))
                if f.get("project_link")
                    .is_some_and(|s| s == &expected || s == &short) => {}
            Ok(None) => {}
            _ => return Ok((0, 1)),
        }
    }
    if io::read(&old)?.as_deref() != Some(&raw) || io::read(&page)?.as_deref() != Some(&desired) {
        return Ok((0, 1));
    }
    Ok((usize::from(io::delete(&old)?), 0))
}
