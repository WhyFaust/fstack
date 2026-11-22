use serde_json::json;
use std::path::{Path, PathBuf};

mod coordinator;
mod gitlab_ci;
mod installation;
mod package;
mod release;
mod review;
mod version;

type Result<T> = std::result::Result<T, String>;

fn main_result() -> Result<()> {
    let argv: Vec<_> = std::env::args_os().skip(1).collect();
    let (package, arguments) = if argv.first().is_some_and(|a| a == "--package") {
        package::require(argv.len() >= 3, "После --package нужен каталог и команда")?;
        (Some(Path::new(&argv[1])), &argv[2..])
    } else {
        (None, argv.as_slice())
    };
    let command = arguments.first().and_then(|s| s.to_str()).unwrap_or("");
    if command == "doctor" {
        return installation::doctor::run(&arguments[1..]);
    }
    let root = package_root(package)?;
    match (command, arguments.len()) {
        ("gitlab-ci-action", 1) => return gitlab_ci::run(std::io::stdin().lock()),
        ("hook", _) => return coordinator::run(&arguments[1..], &root),
        ("release", _) => return release::run(&arguments[1..], &root),
        ("review", _) => return review::run(&arguments[1..]),
        ("check-installation", _) => return installation::run(&arguments[1..], &root),
        ("check-package", 2) => println!("{}", package::check(Path::new(&arguments[1]))?),
        ("resources", 3) => package::resources(Path::new(&arguments[2]), Path::new(&arguments[1]))?,
        ("metadata", 2) => println!("{}", package::metadata(Path::new(&arguments[1]))?),
        _ => return Err("Использование: fstack [--package ROOT] doctor [--codex PATH] [--json] | hook EVENT | gitlab-ci-action | release | review [JSON] | check-installation | check-package ROOT | resources ROOT FOLDER | metadata PATH".into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = main_result() {
        eprintln!("Проверка не пройдена: {error}");
        std::process::exit(1);
    }
}

fn package_root(package: Option<&Path>) -> Result<PathBuf> {
    if let Some(root) = package {
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
