use serde_json::json;
use std::path::{Path, PathBuf};

mod gitlab_ci;
mod installation;
mod memory;
mod package;
mod release;
mod review;
mod version;

type Result<T> = std::result::Result<T, String>;

fn main_result() -> Result<()> {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let root = package_root(&mut arguments)?;
    let command = arguments.first().and_then(|s| s.to_str()).unwrap_or("");
    match (command, arguments.len()) {
        ("gitlab-ci-action", 1) => return gitlab_ci::run(std::io::stdin().lock()),
        ("memory", _) => return memory::run(&arguments[1..], &root),
        ("release", _) => return release::run(&arguments[1..], &root),
        ("review", _) => return review::run(&arguments[1..]),
        ("check-installation", _) => return installation::run(&arguments[1..], &root),
        ("check-package", 2) => println!("{}", package::check(Path::new(&arguments[1]))?),
        ("resources", 3) => package::resources(Path::new(&arguments[2]), Path::new(&arguments[1]))?,
        ("metadata", 2) => println!("{}", package::metadata(Path::new(&arguments[1]))?),
        ("structure", 2) => package::skill_structure(Path::new(&arguments[1]))?,
        _ => return Err("Использование: fstack [--package ROOT] memory | gitlab-ci-action | release | review [JSON] | check-installation | check-package ROOT | resources ROOT FOLDER | metadata PATH | structure PATH".into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = main_result() {
        eprintln!("Проверка не пройдена: {error}");
        std::process::exit(1);
    }
}

fn package_root(arguments: &mut Vec<std::ffi::OsString>) -> Result<PathBuf> {
    if arguments.first().is_some_and(|a| a == "--package") {
        package::require(
            arguments.len() >= 3,
            "После --package нужен каталог и команда",
        )?;
        let root = PathBuf::from(arguments.remove(1));
        arguments.remove(0);
        return root.canonicalize().map_err(|e| e.to_string());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    enclosing_package(&executable)
        .map(Path::to_path_buf)
        .ok_or_else(|| "Не найден пакет рядом с исполняемым файлом; укажите --package ROOT".into())
}

fn enclosing_package(executable: &Path) -> Option<&Path> {
    executable
        .ancestors()
        .skip(1)
        .find(|path| path.join(".codex-plugin/plugin.json").is_file())
}
