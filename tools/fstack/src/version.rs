use crate::Result;
use regex::Regex;
use std::sync::LazyLock;

static STABLE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z").unwrap());

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
