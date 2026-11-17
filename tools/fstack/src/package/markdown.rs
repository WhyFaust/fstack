use super::{contained, excluded_dir, read, require, Result, EXCLUDED_DIRECTORIES};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use regex::{Captures, Regex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

fn parser(text: &str) -> Parser<'_> {
    Parser::new_ext(
        text,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_GFM,
    )
}

pub(super) fn targets(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (event, span) in parser(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Link {
                link_type: LinkType::Email,
                ..
            }) => (),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                found.push(dest_url.into_string());
            }
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {
                html_attributes(&text[span], |tag, name, value| {
                    if (tag.eq_ignore_ascii_case("a") && name.eq_ignore_ascii_case("href"))
                        || (tag.eq_ignore_ascii_case("img") && name.eq_ignore_ascii_case("src"))
                    {
                        found.push(value.to_owned());
                    }
                });
            }
            _ => (),
        }
    }
    found
}

fn html_attributes(html: &str, mut visit: impl FnMut(&str, &str, &str)) {
    let tags =
        rx!(r#"(?is)<!--.*?(?:-->|\z)|<([a-z][a-z0-9:-]*)\b((?:[^<>"']|"[^"]*"|'[^']*')*)>"#);
    let attributes =
        rx!(r#"(?i)\s+([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>\x60]+)))?"#);
    for tag in tags.captures_iter(html) {
        let Some(name) = tag.get(1) else { continue };
        for attr in attributes.captures_iter(&tag[2]) {
            if let Some(value) = (2..=4).find_map(|i| attr.get(i)) {
                let value = rx!(r"&(?:#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);")
                    .replace_all(value.as_str(), |entity: &Captures<'_>| {
                        parser(&entity[0])
                            .find_map(|event| match event {
                                Event::Text(value) => Some(value.into_string()),
                                _ => None,
                            })
                            .unwrap_or_else(|| entity[0].to_owned())
                    });
                visit(name.as_str(), &attr[1], &value);
            }
        }
    }
}

fn sections(text: &str) -> HashSet<String> {
    let mut headings = HashSet::new();
    let mut next_suffix = HashMap::new();
    let mut explicit = HashSet::new();
    let mut heading = None;
    let mut image_depth = 0;
    for (event, span) in parser(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                let title = heading.take().unwrap();
                let title = title.trim().to_lowercase();
                let base = rx!(r"[^\p{Alphabetic}\p{M}\p{Nd}\p{Nl}\p{Pc} -]+")
                    .replace_all(&title, "")
                    .replace(' ', "-");
                let suffix = next_suffix.entry(base.clone()).or_insert(0usize);
                let mut anchor = base.clone();
                while !headings.insert(anchor) {
                    *suffix += 1;
                    anchor = format!("{base}-{suffix}");
                }
            }
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth -= 1,
            Event::Text(value) | Event::Code(value) if image_depth == 0 => {
                if let Some(title) = &mut heading {
                    title.push_str(&value);
                }
            }
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {
                html_attributes(&text[span], |tag, name, value| {
                    if name.eq_ignore_ascii_case("id")
                        || (tag.eq_ignore_ascii_case("a") && name.eq_ignore_ascii_case("name"))
                    {
                        explicit.insert(value.to_owned());
                        explicit.insert(value.to_lowercase());
                    }
                });
            }
            _ => (),
        }
    }
    headings.extend(explicit);
    headings
}

pub(super) fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("md") || v.eq_ignore_ascii_case("markdown"))
}

pub(super) struct Links {
    root: PathBuf,
    sections: HashMap<PathBuf, HashSet<String>>,
}

impl Links {
    pub(super) fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.canonicalize().map_err(|e| e.to_string())?,
            sections: HashMap::new(),
        })
    }

    pub(super) fn read_document(&self, path: &Path) -> Result<String> {
        let file = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let relative = file
            .strip_prefix(&self.root)
            .map_err(|_| format!("{}: документ вне пакета", path.display()))?;
        require(
            !relative.iter().any(|part| {
                part.to_str()
                    .is_some_and(|v| excluded_dir(v, EXCLUDED_DIRECTORIES))
            }),
            format!("{}: документ в служебном каталоге", path.display()),
        )?;
        read(&file)
    }

    pub(super) fn check(&mut self, source: &Path, text: &str) -> Result<()> {
        for target in targets(text) {
            self.check_target(source, &target)
                .map_err(|reason| format!("{}: ссылка «{target}»: {reason}", source.display()))?;
        }
        Ok(())
    }

    fn check_target(&mut self, source: &Path, target: &str) -> Result<()> {
        if target.starts_with("//")
            || (rx!(r"^[a-zA-Z][a-zA-Z0-9+.-]*:").is_match(target)
                && !rx!(r"^[a-zA-Z]:[/\\]").is_match(target))
        {
            return Ok(());
        }
        let (location, fragment) = target.split_once('#').unwrap_or((target, ""));
        let location = location.split('?').next().unwrap_or_default();
        let parent = source.parent().ok_or("Нет родительского каталога")?;
        let file = if location.is_empty() {
            contained(
                &self.root,
                parent,
                source.file_name().ok_or("Нет имени файла")?,
                false,
            )?
        } else if let Some(relative) = location.strip_prefix('/') {
            contained(&self.root, &self.root, super::unquote(relative), true)?
        } else {
            contained(&self.root, parent, super::unquote(location), true)?
        };
        if !fragment.is_empty() && is_markdown(&file) && file.is_file() {
            if !self.sections.contains_key(&file) {
                let document = self.read_document(&file)?;
                self.sections.insert(file.clone(), sections(&document));
            }
            let anchor = super::unquote(fragment);
            require(
                self.sections[&file].contains(&anchor),
                format!("в {} нет раздела «{anchor}»", file.display()),
            )?;
        }
        Ok(())
    }
}
