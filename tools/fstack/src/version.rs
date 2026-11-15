use crate::Result;
use regex::Regex;
use std::sync::LazyLock;

static STABLE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z").unwrap());
static COMMIT_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A([a-zA-Z]+)(?:\([^\r\n)]+\))?(!)?:[ \t]+\S").unwrap());
static BREAKING_FOOTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^BREAKING(?: |-)CHANGE:[ \t]+\S").unwrap());

pub(crate) fn parse(value: &str) -> Result<[u64; 3]> {
    if !STABLE_VERSION.is_match(value) {
        return Err(format!("Некорректная версия: {value}"));
    }
    let parts: Vec<_> = value
        .split('.')
        .map(|n| n.parse::<u64>().map_err(|e| e.to_string()))
        .collect::<Result<_>>()?;
    Ok([parts[0], parts[1], parts[2]])
}
pub(crate) fn increment(value: &str, level: u8) -> Result<String> {
    let [a, b, c] = parse(value)?;
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
pub(crate) fn change_level(messages: &str) -> u8 {
    messages
        .split('\0')
        .map(|message| {
            let parsed = COMMIT_HEADER.captures(message.trim());
            if parsed.as_ref().is_some_and(|c| c.get(2).is_some())
                || BREAKING_FOOTER.is_match(message)
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

pub(crate) fn check_series(first: &str, current: &str) -> Result<()> {
    let start = parse(first)?;
    let current = parse(current)?;
    if current[0] != start[0] || current < start {
        return Err(format!(
            "Версия пакета должна принадлежать серии от {first}"
        ));
    }
    Ok(())
}

pub(crate) fn series_version(
    previous: &str,
    before: Option<&str>,
    after: Option<&str>,
    level: u8,
) -> Result<String> {
    let Some(first) = after else {
        return if before.is_none() {
            increment(previous, level)
        } else {
            Err("Нельзя удалить launchers/package-series".into())
        };
    };
    if let Some(start) = before {
        check_series(start, previous)?;
        if start == first {
            if level == 3 && parse(first)?[0] != 0 {
                return Err("Для несовместимого изменения укажите новую серию".into());
            }
            return increment(previous, level.min(2));
        }
    }
    let previous_major = parse(previous)?[0];
    let next_major = parse(first)?[0];
    let reset = before.is_none() && previous_major > 0 && next_major == 0;
    if level != 3 || (!reset && next_major <= previous_major) {
        return Err("Переход на другую серию требует явной версии и breaking change".into());
    }
    Ok(first.into())
}
