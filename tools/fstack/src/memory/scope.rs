use super::{io, Result};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn git(executable: &Path, cwd: &Path, args: &[&str]) -> Result<Option<Vec<u8>>> {
    let mut cmd = Command::new(executable);
    cmd.args(["-C"])
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().to_uppercase().starts_with("GIT_") {
            cmd.env_remove(k);
        }
    }
    cmd.env(
        "GIT_CONFIG_GLOBAL",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
    )
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_OPTIONAL_LOCKS", "0");
    let mut child = cmd.spawn().map_err(io::err)?;
    let stdout = child.stdout.take().ok_or("Missing Git output")?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.take(1_048_577).read_to_end(&mut b).map(|_| b)
    });
    let end = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(io::err)? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cannot establish the Git memory scope".into());
        }
        std::thread::sleep(Duration::from_millis(10))
    };
    let out = reader
        .join()
        .map_err(|_| "Cannot read Git output")?
        .map_err(io::err)?;
    if out.len() > 1_048_576 {
        return Err("Git memory scope exceeds limit".into());
    }
    Ok(status.success().then_some(out))
}
pub fn identity(cwd: &Path) -> Result<String> {
    let cwd = io::absolute(cwd)?;
    if !io::directory(&cwd, false, false)? {
        return Err("Working directory does not exist".into());
    }
    let mut marker = None;
    for p in cwd.ancestors() {
        if io::info(&p.join(".git"))?.is_some() {
            marker = Some(p.to_owned());
            break;
        }
    }
    let Some(marker) = marker else {
        return Ok(format!("dir:{}", io::normalized(&cwd)));
    };
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let exe = executable(&marker, &paths)?;
    let out = git(
        &exe,
        &cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
    )?
    .ok_or("Cannot establish the Git memory scope")?;
    let text = String::from_utf8(out).map_err(io::err)?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 2 {
        return Err("Invalid Git memory scope".into());
    }
    let root = io::absolute(Path::new(lines[0]))?;
    let common = io::absolute(Path::new(lines[1]))?;
    if !io::equal(&root, &marker) || !io::directory(&common, false, false)? {
        return Err("Invalid Git memory scope".into());
    }
    let worktrees = git(&exe, &cwd, &["worktree", "list", "--porcelain", "-z"])?
        .ok_or("Invalid Git worktrees")?;
    let text = String::from_utf8(worktrees).map_err(io::err)?;
    if !text
        .split('\0')
        .filter_map(|s| s.strip_prefix("worktree "))
        .any(|p| {
            io::absolute(Path::new(p))
                .ok()
                .is_some_and(|p| io::equal(&p, &root))
        })
    {
        return Err("Working directory is not a registered Git worktree".into());
    }
    Ok(format!("git:{}", io::normalized(&common)))
}
pub fn binding_path(state: &Path, cwd: &Path) -> Result<PathBuf> {
    Ok(state.join(format!("{}.json", io::hash(identity(cwd)?))))
}

pub fn executable(marker: &Path, paths: &std::ffi::OsStr) -> Result<PathBuf> {
    let physical_marker = marker.canonicalize().map_err(io::err)?;
    std::env::split_paths(paths)
        .filter(|p| p.is_absolute() && !io::within(p, marker))
        .find_map(|p| {
            let p = p
                .join(if cfg!(windows) { "git.exe" } else { "git" })
                .canonicalize()
                .ok()?;
            let metadata = p.metadata().ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return None;
                }
            }
            (!io::within(&p, &physical_marker) && metadata.is_file()).then_some(p)
        })
        .ok_or("Cannot find Git outside the worktree".into())
}
