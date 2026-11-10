use super::Result;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
pub fn hash(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data.as_ref()))
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    if path.to_str().is_none() {
        return Err("Memory paths require UTF-8".into());
    }
    let p = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(err)?.join(path)
    };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(c),
        }
    }
    Ok(out)
}
#[cfg(windows)]
fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = if let Some(s) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{s}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    };
    text.to_lowercase()
}
pub fn normalized(path: &Path) -> String {
    #[cfg(windows)]
    {
        path_key(path)
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}
pub fn equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        path_key(left) == path_key(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
pub fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path_key(path);
        let root = path_key(root);
        path == root || path.starts_with(&(root.trim_end_matches('\\').to_owned() + "\\"))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}
pub fn err(_: impl std::fmt::Display) -> String {
    "Local memory operation failed".into()
}
pub fn info(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Err("Symbolic links are not allowed in memory paths".into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    return Err("Reparse points are not allowed in memory paths".into());
                }
            }
            Ok(Some(m))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(err(e)),
    }
}
pub fn directory(path: &Path, create: bool, private: bool) -> Result<bool> {
    let mut parents = path.ancestors().collect::<Vec<_>>();
    parents.reverse();
    for p in parents {
        if p.as_os_str().is_empty() {
            continue;
        }
        let mut m = info(p)?;
        if m.is_none() {
            if !create {
                return Ok(false);
            }
            let mut b = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(if private { 0o700 } else { 0o777 });
            }
            match b.create(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(err(e)),
            }
            m = info(p)?;
        }
        if !m.is_some_and(|m| m.is_dir()) {
            return Err("Memory path is not a directory".into());
        }
    }
    Ok(true)
}
pub fn file(path: &Path) -> Result<bool> {
    if !directory(path.parent().ok_or("Missing parent")?, false, false)? {
        return Ok(false);
    }
    match info(path)? {
        None => Ok(false),
        Some(m) if m.is_file() => Ok(true),
        _ => Err("Memory path is not a regular file".into()),
    }
}
pub fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    o
}
pub fn read_limit(path: &Path, limit: usize) -> Result<Option<String>> {
    if !file(path)? {
        return Ok(None);
    }
    let f = match options().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e)),
    };
    if !f.metadata().map_err(err)?.is_file() {
        return Err("Memory path is not a regular file".into());
    }
    let mut b = Vec::new();
    f.take((limit + 1) as u64)
        .read_to_end(&mut b)
        .map_err(err)?;
    if b.len() > limit {
        return Err("Memory file is too large".into());
    }
    let s = String::from_utf8(b).map_err(|_| "Invalid encoding in memory file")?;
    Ok(Some(s.trim_start_matches('\u{feff}').replace("\r\n", "\n")))
}
pub fn prefix(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    if !file(path)? {
        return Ok(None);
    }
    let f = options().read(true).open(path).map_err(err)?;
    if !f.metadata().map_err(err)?.is_file() {
        return Err("Memory path is not a regular file".into());
    }
    let mut data = Vec::new();
    f.take(limit as u64).read_to_end(&mut data).map_err(err)?;
    Ok(Some(data))
}
pub fn read(path: &Path) -> Result<Option<String>> {
    read_limit(path, 32768)
}
pub fn nonce() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(err)?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
pub fn write(path: &Path, text: &str, replace: bool, expected: Option<&str>) -> Result<bool> {
    if !directory(path.parent().ok_or("Missing parent")?, false, false)? {
        return Err("Missing memory directory".into());
    }
    if file(path)? && !replace {
        return Ok(false);
    }
    let temp = path.parent().unwrap().join(format!(".fstack-{}", nonce()?));
    let result = (|| {
        let mut f = options()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(err)?;
        f.write_all(text.as_bytes()).map_err(err)?;
        f.sync_all().map_err(err)?;
        if expected.is_some() && read(path)?.as_deref() != expected {
            return Err("Memory note changed while preparing an update".into());
        }
        file(path)?;
        let r = if replace {
            fs::rename(&temp, path)
        } else {
            publish_new(&temp, path)
        };
        match r {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                file(path)?;
                Ok(false)
            }
            Err(e) => Err(err(e)),
        }
    })();
    let _ = fs::remove_file(&temp);
    result
}
#[cfg(unix)]
fn publish_new(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::hard_link(source, target)
}
#[cfg(windows)]
fn publish_new(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(source.as_ptr(), target.as_ptr(), 0)
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn delete(path: &Path) -> Result<bool> {
    if !file(path)? {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(err(e)),
    }
}
pub fn home() -> Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("Home directory unavailable".into())
}
pub fn lock(project: &Path, vault: &Path) -> Result<File> {
    #[cfg(test)]
    let cache = super::tests::cache();
    #[cfg(not(test))]
    let cache = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_CACHE_HOME"
    })
    .map(PathBuf::from)
    .unwrap_or(home()?.join(".cache"));
    if !cache.is_absolute() {
        return Err("Memory locks require an absolute cache directory".into());
    }
    let dir = absolute(&cache.join("fstack/memory-locks"))?;
    if within(&dir, vault) {
        return Err("Local memory lock directory is inside the vault".into());
    }
    directory(project, false, false)?;
    #[cfg(unix)]
    let m = info(project)?.ok_or("Memory project unavailable")?;
    #[cfg(unix)]
    let key = {
        use std::os::unix::fs::MetadataExt;
        hash(format!("{}:{}", m.dev(), m.ino()))
    };
    #[cfg(windows)]
    let key = {
        let (volume, index, _) = windows_info(project, true)?;
        hash(format!("{volume}:{index}"))
    };
    directory(&dir, true, true)?;
    let p = dir.join(format!("{key}.lock"));
    file(&p)?;
    let f = options()
        .create(true)
        .read(true)
        .write(true)
        .open(p)
        .map_err(err)?;
    if !f.metadata().map_err(err)?.is_file() {
        return Err("Unsafe memory lock".into());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match fs2::FileExt::try_lock_exclusive(&f) {
            Ok(()) => return Ok(f),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return Err("Timed out waiting for the local memory lock".into()),
        }
    }
}
pub fn state(dir: &Path, cwd: &Path, create: bool) -> Result<bool> {
    if within(dir, cwd) {
        return Err("Memory hook state must be outside the workspace".into());
    }
    for p in dir.ancestors() {
        if info(&p.join(".git"))?.is_some() {
            return Err("Memory hook state must be outside the workspace".into());
        }
    }
    directory(dir, create, true)
}

#[cfg(windows)]
pub fn windows_info(path: &Path, directory: bool) -> Result<(u32, u64, u32)> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let mut opts = OpenOptions::new();
    opts.read(true).custom_flags(
        FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            },
    );
    let f = opts.open(path).map_err(err)?;
    let mut metadata = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(f.as_raw_handle(), metadata.as_mut_ptr()) } == 0 {
        return Err("Cannot inspect physical memory file".into());
    }
    let m = unsafe { metadata.assume_init() };
    if m.dwFileAttributes & 0x400 != 0 {
        return Err("Reparse points are not allowed in memory paths".into());
    }
    Ok((
        m.dwVolumeSerialNumber,
        ((m.nFileIndexHigh as u64) << 32) | m.nFileIndexLow as u64,
        m.nNumberOfLinks,
    ))
}
