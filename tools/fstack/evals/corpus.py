"""Воспроизводимые задачи из истории репозитория, без копии эталона у решателя."""
from __future__ import annotations

import hashlib
import io
import json
import os
import re
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[3]


def check_storage(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".storage-check-", dir=directory) as temporary:
        path = Path(temporary) / "mode"
        path.write_bytes(b"fixture")
        if os.name != "nt":
            path.chmod(0o600)
            if path.stat().st_mode & 0o777 != 0o600:
                raise ValueError("Каталог результатов не сохраняет права файлов. В WSL используйте Linux filesystem, например /home, а не /mnt/c.")


def digest(value) -> str:
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(",", ":")).encode()).hexdigest()


def git(repo: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)


def resolve(repo: Path, ref: str) -> str:
    return git(repo, "rev-parse", "--verify", "--end-of-options", f"{ref}^{{commit}}").decode().strip()


def snapshot(root: Path) -> dict:
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink() or getattr(path, "is_junction", lambda: False)():
            raise ValueError(f"Ссылка в артефакте: {path}")
        if path.is_file() and "__pycache__" not in path.parts:
            result[path.relative_to(root).as_posix()] = {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                                        "mode": path.stat().st_mode & 0o777}
    return result


def extract(repo: Path, ref: str, target: Path, paths: list[str]) -> dict[str, str]:
    target.mkdir(parents=True, exist_ok=False)
    data = git(repo, "archive", "--format=tar", ref, "--", *paths)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            relative = PurePosixPath(member.name)
            if (relative.is_absolute() or ".." in relative.parts or "\\" in member.name
                    or any(":" in p or p.rstrip(". ") != p or re.fullmatch(r"(?i)(con|prn|aux|nul|com[0-9]|lpt[0-9])(?:\..*)?", p)
                           for p in relative.parts)):
                raise ValueError("Небезопасный путь в Git archive")
            if not (member.isdir() or member.isfile()):
                raise ValueError("Корпус не допускает ссылки и специальные файлы")
            path = target.joinpath(*relative.parts)
            if not path.resolve().is_relative_to(target.resolve()):
                raise ValueError("Путь вышел за корень снимка")
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(archive.extractfile(member).read())
                path.chmod(member.mode & 0o777)
    return snapshot(target)


def cases(bank: Path) -> list[dict]:
    data = json.loads(bank.read_text(encoding="utf-8"))
    seen = set()
    for case in data["cases"]:
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", case["id"]) or case["id"] in seen or not case["prompt"].strip():
            raise ValueError("Повтор ID или пустое поручение")
        seen.add(case["id"])
        for key in ("base", "reference"):
            if len(case[key]) != 40 or any(c not in "0123456789abcdef" for c in case[key]):
                raise ValueError("Корпус требует полные SHA коммитов")
        if case["split"] not in {"development", "holdout", "control"}:
            raise ValueError("Неизвестная группа задач")
        failure = case.get("before_failure")
        if (not isinstance(failure, list) or any(not isinstance(x, str) or not x.strip() for x in failure)
                or bool(failure) != (case["split"] != "control")):
            raise ValueError("Для изменяемого поведения нужен известный исходный отказ; для контроля список пуст")
        if case["oracle_file"] not in {"scripts/tests/test_memory_store.py", "scripts/tests/test_memory_hooks.py"}:
            raise ValueError("Неизвестный проверяющий модуль")
        if not re.fullmatch(r"scripts\.tests\.test_memory_(store|hooks)\.Memory(Store|Hook)Tests\.test_[a-z0-9_]+", case["test"]):
            raise ValueError("Неизвестный формат проверки")
    return data["cases"]


def materialize(repo: Path, case: dict, target: Path, reference: bool = False):
    return extract(repo, case["reference"] if reference else case["base"], target, ["."])


def grade(repo: Path, case: dict, workspace: Path, command_prefix: list[str] = (), timeout: int = 90,
          home: Path | None = None) -> dict:
    # Вызывается только на отдельной копии результата после остановки решателя.
    path = case["oracle_file"]
    oracle = git(repo, "show", f'{case["reference"]}:{path}')
    (workspace / path).parent.mkdir(parents=True, exist_ok=True)
    (workspace / path).write_bytes(oracle)
    # -I исключает sitecustomize и modules из cwd; грузить закреплённый файл напрямую.
    test_name = ".".join(case["test"].split(".")[-2:])
    bootstrap = ("import runpy,types,unittest,sys; "
                 "scope=types.SimpleNamespace(**runpy.run_path(sys.argv[1])); "
                 "suite=unittest.defaultTestLoader.loadTestsFromName(sys.argv[2],scope); "
                 "result=unittest.TextTestRunner(verbosity=2).run(suite); "
                 "sys.exit(0 if result.wasSuccessful() and result.testsRun == 1 and not result.skipped else 1)")
    command = [*command_prefix, sys.executable, "-I", "-B", "-c", bootstrap, str(workspace / path), test_name]
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "HOME", "USERPROFILE", "LANG", "CODEX_HOME"}}
    scratch = workspace.with_name(workspace.name + "-scratch")
    scratch.mkdir(exist_ok=True)
    env.update(TMP=str(scratch), TEMP=str(scratch), TMPDIR=str(scratch),
               HOME=str(scratch), USERPROFILE=str(scratch), LOCALAPPDATA=str(scratch),
               XDG_CACHE_HOME=str(scratch), PYTHONIOENCODING="utf-8")
    if home is not None:
        env["CODEX_HOME"] = str(home)
    else:
        env.pop("CODEX_HOME", None)
    try:
        proc = subprocess.run(command, cwd=workspace, env=env, capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout)
        output = proc.stdout + proc.stderr
        ran = "Ran 1 test" in output and "skipped=" not in output
        # Отсутствующая зависимость/сбой запуска не считаются ошибкой решения.
        unavailable = (not ran and not ("Traceback (most recent call last):" in output)) or "skipped=" in output
        return {"status": "unavailable" if unavailable else "pass" if proc.returncode == 0 else "fail",
                "exit_code": proc.returncode, "output": output, "oracle_sha256": hashlib.sha256(oracle).hexdigest()}
    except subprocess.TimeoutExpired:
        return {"status": "unavailable", "error": "Превышено время проверки"}


def valid_pair(case: dict, base: dict, reference: dict) -> bool:
    if reference["status"] != "pass":
        return False
    if case["split"] == "control":
        return base["status"] == "pass"
    return (base["status"] == "fail" and bool(case["before_failure"])
            and all(fragment in base.get("output", "") for fragment in case["before_failure"]))


def validate(repo: Path, bank: Path, output: Path):
    output.mkdir(parents=True, exist_ok=False)
    check_storage(output)
    results = []
    for case in cases(bank):
        item = {"id": case["id"], "split": case["split"], "case_sha256": digest(case)}
        for version in ("base", "reference"):
            workspace = output / case["id"] / version
            materialize(repo, case, workspace, version == "reference")
            item[version] = grade(repo, case, workspace)
        item["valid"] = valid_pair(case, item["base"], item["reference"])
        results.append(item)
        print(f'{case["id"]}: {item["base"]["status"]} -> {item["reference"]["status"]}', flush=True)
    (output / "validation.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return all(x["valid"] for x in results)
