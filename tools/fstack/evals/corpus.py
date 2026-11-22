"""Воспроизводимые задачи с закреплёнными снимками и отдельной проверкой."""
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

import unittest_check

ROOT = Path(__file__).resolve().parents[3]
PYTHON_AQA_ORACLE = "oracle.py"
PYTHON_AQA_SOURCES = ["dispatch.py", "checks.py"]
# Путь, класс и модули решения задаёт runner. Банк выбирает только проверку.
ORACLES = {
    "scripts/tests/test_memory_store.py": ("scripts.tests.test_memory_store.MemoryStoreTests", ("memory_store",)),
    "scripts/tests/test_memory_hooks.py": ("scripts.tests.test_memory_hooks.MemoryHookTests",
                                           ("memory", "memory_store", "hook_spool")),
    PYTHON_AQA_ORACLE: ("oracle.DispatchContractTests", ("checks", "dispatch")),
}


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
            if not isinstance(case[key], str) or not re.fullmatch(r"[0-9a-f]{40}", case[key]):
                raise ValueError("Корпус требует полные SHA снимков Git")
        if case["split"] not in {"development", "holdout", "control"}:
            raise ValueError("Неизвестная группа задач")
        failure = case.get("before_failure")
        if (not isinstance(failure, list) or any(not isinstance(x, str) or not x.strip() for x in failure)
                or bool(failure) != (case["split"] != "control")):
            raise ValueError("Для изменяемого поведения нужен известный исходный отказ; для контроля список пуст")
        oracle_test(case)
    return data["cases"]


def oracle_test(case: dict) -> tuple[str, str, tuple[str, ...]]:
    path = case["oracle_file"]
    if path not in ORACLES:
        raise ValueError("Неизвестный проверяющий модуль")
    qualified_class, candidate_modules = ORACLES[path]
    if not re.fullmatch(re.escape(qualified_class) + r"\.test_[a-z0-9_]+", case["test"]):
        raise ValueError("Неизвестный формат проверки")
    return qualified_class.rsplit(".", 1)[1], case["test"].rsplit(".", 1)[1], candidate_modules


def materialize(repo: Path, case: dict, target: Path, reference: bool = False):
    revision = case["reference"] if reference else case["base"]
    if case["oracle_file"] == PYTHON_AQA_ORACLE:
        if git(repo, "cat-file", "-t", revision).strip() != b"tree":
            raise ValueError("Сценарий Python/AQA требует Git SHA дерева")
        return extract(repo, revision, target, PYTHON_AQA_SOURCES)
    if resolve(repo, revision) != revision:
        raise ValueError("Исторический случай требует Git SHA коммита")
    return extract(repo, revision, target, ["."])


def grade(repo: Path, case: dict, workspace: Path, command_prefix: list[str] = (), timeout: int = 90,
          home: Path | None = None) -> dict:
    # Проверяет отдельный снимок. Артефакт решателя копируется сюда после его остановки.
    class_name, test_name, candidate_modules = oracle_test(case)
    workspace = workspace.resolve()
    path = case["oracle_file"]
    scratch = workspace.with_name(workspace.name + "-scratch")
    try:
        oracle = git(repo, "show", f'{case["reference"]}:{path}')
        (workspace / path).parent.mkdir(parents=True, exist_ok=True)
        (workspace / path).write_bytes(oracle)
        scratch.mkdir(exist_ok=True)
        bootstrap = Path(unittest_check.__file__).read_text(encoding="utf-8")
    except (OSError, subprocess.SubprocessError) as exc:
        return {"status": "unavailable", "error": str(exc)}
    # Изолированный Python загружает закреплённый файл по пути, даже если решатель добавил одноимённый модуль.
    command = [*command_prefix, sys.executable, "-I", "-B", "-X", "utf8", "-c", bootstrap,
               str(workspace / path), class_name, test_name, *candidate_modules]
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "LANG"}}
    env.update(TMP=str(scratch), TEMP=str(scratch), TMPDIR=str(scratch),
               HOME=str(scratch), USERPROFILE=str(scratch), LOCALAPPDATA=str(scratch),
               XDG_CACHE_HOME=str(scratch), PYTHONIOENCODING="utf-8")
    if home is not None:
        env["CODEX_HOME"] = str(home)
    try:
        proc = subprocess.run(command, cwd=workspace, env=env, capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"status": "unavailable", "error": "Превышено время проверки"}
    except OSError as exc:
        return {"status": "unavailable", "error": str(exc)}
    prefix = unittest_check.RESULT_PREFIX
    statuses = [line.removeprefix(prefix) for line in proc.stdout.splitlines() if line.startswith(prefix)]
    status = "unavailable"
    if len(statuses) == 1 and unittest_check.EXIT_CODES.get(statuses[0]) == proc.returncode:
        status = statuses[0]
    return {"status": status, "exit_code": proc.returncode, "output": proc.stdout + proc.stderr,
            "oracle_sha256": hashlib.sha256(oracle).hexdigest()}


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
            try:
                materialize(repo, case, workspace, version == "reference")
                item[version] = grade(repo, case, workspace)
            except (OSError, ValueError, subprocess.SubprocessError) as exc:
                item[version] = {"status": "unavailable", "error": str(exc)}
        item["valid"] = valid_pair(case, item["base"], item["reference"])
        results.append(item)
        print(f'{case["id"]}: {item["base"]["status"]} -> {item["reference"]["status"]}', flush=True)
    (output / "validation.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return bool(results) and all(x["valid"] for x in results)
