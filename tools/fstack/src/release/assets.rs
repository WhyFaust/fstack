use super::{binary_name, rx, Release, Result, TARGETS};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

impl Release<'_> {
    pub(super) fn artifacts(&self, sha: &str) -> Result<Vec<(String, Vec<u8>)>> {
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

    pub(super) fn verify_assets(
        &self,
        repo: &str,
        number: &str,
        source: &str,
        local: Option<&[PathBuf]>,
    ) -> Result<()> {
        let tag = format!("v{number}");
        let release: Value = serde_json::from_slice(&self.bytes(&[
            "gh",
            "api",
            &format!("repos/{repo}/releases/tags/{tag}"),
        ])?)
        .map_err(|e| e.to_string())?;
        if release["tag_name"] != tag || release["draft"] != false || release["prerelease"] != false
        {
            return Err(format!("GitHub не подтвердил опубликованный выпуск {tag}"));
        }
        let id = release["id"].as_u64().ok_or("Нет ID выпуска")?;
        self.verify_inventory(repo, number, source, local, id)
    }

    pub(super) fn verify_inventory(
        &self,
        repo: &str,
        number: &str,
        source: &str,
        local: Option<&[PathBuf]>,
        id: u64,
    ) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct Asset {
            id: u64,
            name: String,
            state: String,
            size: u64,
            digest: String,
        }
        let tag = format!("v{number}");
        let pages: Vec<Vec<Asset>> = serde_json::from_slice(&self.bytes(&[
            "gh",
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/releases/{id}/assets?per_page=100"),
        ])?)
        .map_err(|e| e.to_string())?;
        let mut files = BTreeMap::new();
        let digest = rx(r"\Asha256:[0-9a-f]{64}\z");
        for asset in pages.iter().flatten() {
            if asset.state != "uploaded" || asset.size == 0 || !digest.is_match(&asset.digest) {
                return Err(format!(
                    "Файл {} не загружен полностью или не имеет SHA-256",
                    asset.name
                ));
            }
            if files.insert(asset.name.as_str(), asset).is_some() {
                return Err(format!("Повтор имени файла выпуска: {}", asset.name));
            }
        }
        if files.len() != TARGETS.len() * 4 {
            return Err(format!(
                "Неполный или посторонний набор файлов выпуска {tag}"
            ));
        }
        let manifest = self.manifest_at(source)?;
        let name = manifest["name"].as_str().ok_or("Нет имени пакета")?;
        let runtime_id = self.check_runtime_id(source)?;
        for target in TARGETS {
            let suffix = if target.contains("windows") {
                ".exe"
            } else {
                ""
            };
            for filename in [
                format!("{name}-{number}-{target}.zip"),
                format!("{name}-{number}-{runtime_id}-{target}{suffix}"),
            ] {
                let asset = files
                    .get(filename.as_str())
                    .ok_or_else(|| format!("Нет файла {filename} в выпуске"))?;
                let checksum_name = format!("{filename}.sha256");
                let checksum = files
                    .get(checksum_name.as_str())
                    .ok_or_else(|| format!("Нет файла {checksum_name} в выпуске"))?;
                let expected = format!("{}  {filename}\n", &asset.digest["sha256:".len()..]);
                if checksum.size != expected.len() as u64
                    || checksum.digest
                        != format!("sha256:{:x}", Sha256::digest(expected.as_bytes()))
                {
                    return Err(format!(
                        "Контрольная сумма {checksum_name} не соответствует {filename}"
                    ));
                }
                let contents = self.bytes(&[
                    "gh",
                    "api",
                    "-H",
                    "Accept: application/octet-stream",
                    &format!("repos/{repo}/releases/assets/{}", checksum.id),
                ])?;
                if contents != expected.as_bytes() {
                    return Err(format!(
                        "Содержимое {checksum_name} не подтверждает {filename}"
                    ));
                }
            }
        }
        if let Some(local) = local {
            for path in local {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("Нет имени локального файла")?;
                let asset = files.remove(name).ok_or_else(|| {
                    format!("Файл {name} отсутствует в выпуске или повторяется локально")
                })?;
                let bytes = fs::read(path).map_err(|e| e.to_string())?;
                if asset.size != bytes.len() as u64
                    || asset.digest != format!("sha256:{:x}", Sha256::digest(&bytes))
                {
                    return Err(format!(
                        "Опубликованный файл {name} отличается от проверенной сборки"
                    ));
                }
            }
            if !files.is_empty() {
                return Err("Неполный локальный набор файлов для проверки выпуска".into());
            }
        }
        Ok(())
    }
}
