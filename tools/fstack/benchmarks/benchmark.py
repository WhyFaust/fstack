"""Измеряет штатные вызовы координатора с проверкой контекста каждого ответа."""
import argparse
from collections.abc import Callable, Iterable
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
EVENT = "UserPromptSubmit"
LABELS = {
    "cold_inactive": "Обычный запрос с новым профилем",
    "explicit_activation": "Явное включение с новым профилем",
    "warm_continuation": "Продолжение включённого чата",
}
WARMUPS = {"cold_inactive": 0, "explicit_activation": 0, "warm_continuation": 1}
ACTIVE = "В этом чате включён faust-mode до команды /faust-mode-off."
INACTIVE = "Faust-mode выключен."
Observation = dict[str, str | int]


def fingerprint(paths: Iterable[Path], root: Path = ROOT) -> str:
    digest = hashlib.sha256()
    for path in sorted(paths, key=lambda p: p.relative_to(root).as_posix()):
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def inputs() -> dict[str, str]:
    runtime = ROOT / "tools/fstack"
    return {
        "runtime_sha256": fingerprint([
            *runtime.glob("src/**/*.rs"), runtime / "Cargo.toml", runtime / "Cargo.lock",
        ]),
        "package_sha256": fingerprint([
            ROOT / "docs/skills.md", ROOT / ".codex-plugin/plugin.json",
            *ROOT.glob("skills/*/SKILL*.md"),
        ]),
        "harness_sha256": fingerprint([Path(__file__).resolve()]),
    }


def catalog_signals(root: Path = ROOT) -> dict[str, str]:
    signals = {}
    for line in (root / "docs/skills.md").read_text(encoding="utf-8").splitlines():
        row = re.fullmatch(r"\| \[([a-z0-9-]+)\]\(\.\./skills/\1/SKILL\.md\) \| (.+) \|", line)
        if row:
            if row[1] in signals:
                raise ValueError("Каталог содержит повтор навыка")
            signal = re.sub(r"(?:,?\s+|^)\[[^\]]+\]\([^)]+\)$", "", row[2]).strip()
            if not signal:
                raise ValueError("Каталог содержит пустое описание")
            signals[row[1]] = signal
    methods = {path.parent.name for path in root.glob("skills/*/SKILL.md")}
    if not signals or set(signals) != methods:
        raise ValueError("Каталог не совпадает с набором методов пакета")
    return signals


def expect_context(value: object, signals: dict[str, str], root: Path, *,
                   active: bool, index: bool) -> Observation:
    if not isinstance(value, dict) or set(value) != {"hookSpecificOutput"}:
        raise ValueError("Хук вернул ошибку, служебное сообщение или пустой ответ")
    output = value["hookSpecificOutput"]
    if not isinstance(output, dict) or output.get("hookEventName") != EVENT:
        raise ValueError("Хук вернул контекст другого события")
    context = output.get("additionalContext")
    if not isinstance(context, str) or not context:
        raise ValueError("Хук не передал текст контекста")
    if active:
        if not context.startswith(ACTIVE) or INACTIVE in context:
            raise ValueError("Хук не подтвердил постоянное включение координатора")
        for relative in ("skills/faust-mode/SKILL.md", "skills/route/SKILL.md", "docs/skills.md"):
            if str(root / relative) not in context:
                raise ValueError("Хук потерял путь к методу или каталогу пакета")
    elif not context.startswith(INACTIVE) or "включён faust-mode" in context:
        raise ValueError("Обычный запрос самопроизвольно включил координатор")
    entries = [line for line in context.splitlines() if re.fullmatch(r"[a-z0-9-]+ — .+", line)]
    expected = [f"{name} — {signal}" for name, signal in signals.items()] if index else []
    if entries != expected:
        raise ValueError("Контекст содержит неверный, повторный или неполный индекс методов")
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return {
        "response_sha256": hashlib.sha256(encoded).hexdigest(),
        "context_bytes": len(context.encode("utf-8")),
        "index_entries": len(entries),
    }


def summary(samples: list[int | float]) -> dict[str, float]:
    if not samples or any(type(v) not in (int, float) or not math.isfinite(v) or v < 0 for v in samples):
        raise ValueError("Замеры должны быть непустыми, конечными и неотрицательными")
    return {
        "median_ms": round(statistics.median(samples), 3),
        "p95_ms": round(sorted(samples)[math.ceil(len(samples) * 0.95) - 1], 3),
    }


def measure(prepare: Callable[[int], Callable[[], Observation]], samples: int,
            warmups: int = 0) -> dict:
    timings, responses = [], []
    for index in range(-warmups, samples):
        operation = prepare(index)
        started = time.perf_counter_ns()
        response = operation()
        elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
        if index >= 0:
            timings.append(elapsed)
            responses.append(response)
    return {"samples_ms": timings, "responses": responses, "warmups": warmups,
            **summary(timings), "verified": True}


def build(target: str | None) -> Path:
    command = ["cargo", "+1.90.0", "build", "--release", "--locked", "--message-format=json",
               "--manifest-path", str(ROOT / "tools/fstack/Cargo.toml")]
    if target:
        command += ["--target", target]
    completed = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, check=True)
    artifacts = [json.loads(line) for line in completed.stdout.splitlines()]
    binaries = [Path(item["executable"]) for item in artifacts
                if item.get("reason") == "compiler-artifact" and item.get("executable")
                and item["target"]["name"] == "fstack"]
    if len(binaries) != 1:
        raise ValueError("Cargo не вернул единственный исполняемый файл fstack")
    return binaries[0].resolve(strict=True)


class HookRunner:
    def __init__(self, directory: Path, binary: Path, package: Path, signals: dict[str, str]):
        self.binary, self.package, self.signals = binary, package, signals
        self.project = directory / "project"
        self.project.mkdir(parents=True)
        keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP", "LANG", "LC_ALL"}
        self.env = {key: value for key, value in os.environ.items() if key.upper() in keep}
        for name, folder in {
            "CODEX_HOME": "codex", "HOME": "user", "USERPROFILE": "user",
            "APPDATA": "config", "LOCALAPPDATA": "cache",
            "XDG_CONFIG_HOME": "config", "XDG_CACHE_HOME": "cache",
        }.items():
            path = directory / folder
            path.mkdir(exist_ok=True)
            self.env[name] = str(path)

    def invoke(self, prompt: str, turn: str, *, active: bool, index: bool) -> Observation:
        payload = {
            "hook_event_name": EVENT, "session_id": "benchmark", "turn_id": turn,
            "cwd": str(self.project), "prompt": prompt, "permission_mode": "default",
            "collaboration_mode": {"mode": "default"}, "transcript_path": None,
        }
        result = subprocess.run(
            [str(self.binary), "--package", str(self.package), "hook", EVENT],
            input=json.dumps(payload, ensure_ascii=False).encode(),
            capture_output=True, cwd=self.project, env=self.env, timeout=10,
        )
        if result.returncode or result.stderr:
            detail = result.stderr.decode("utf-8", errors="replace").strip()
            raise RuntimeError(f"Вызов координатора завершился с кодом {result.returncode}: {detail}")
        return expect_context(json.loads(result.stdout), self.signals, self.package,
                              active=active, index=index)


def run(args: argparse.Namespace) -> None:
    before = inputs()
    signals = catalog_signals()
    binary = build(args.target)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="fstack-benchmark-") as temporary:
        temporary = Path(temporary).resolve()

        def fresh(name: str, prompt: str, active: bool, index: bool):
            def prepare(sample: int):
                runner = HookRunner(temporary / name / str(sample), binary, ROOT, signals)
                return lambda: runner.invoke(prompt, "measured", active=active, index=index)
            return prepare

        results = {
            "cold_inactive": measure(fresh("inactive", "Проверь модуль.", False, False), args.samples),
            "explicit_activation": measure(fresh("activate", "/faust-mode Проверь модуль.", True, True), args.samples),
        }
        runner = HookRunner(temporary / "continuation", binary, ROOT, signals)
        runner.invoke("/faust-mode Проверь модуль.", "setup", active=True, index=True)
        results["warm_continuation"] = measure(
            lambda sample: lambda: runner.invoke("Продолжи проверку.", f"turn-{sample}", active=True, index=False),
            args.samples, WARMUPS["warm_continuation"],
        )
    if inputs() != before:
        raise ValueError("Исходники или пакет изменились во время замера; повторите запуск")
    if hashlib.sha256(binary.read_bytes()).hexdigest() != binary_hash:
        raise ValueError("Бинарник изменился во время замера; повторите запуск")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT))
    data = {
        "schema": 2, "kind": "coordinator", "created_at": datetime.now(timezone.utc).isoformat(),
        "revision": revision, "tracked_changes": dirty, "inputs": before,
        "binary_sha256": binary_hash,
        "build": {"toolchain": "1.90.0", "profile": "release", "locked": True,
                  "target": args.target, "verified_by": "cargo compiler-artifact from measured sources"},
        "environment": {"system": platform.system(), "release": platform.release(),
                        "machine": platform.machine(), "processor": platform.processor(),
                        "python": platform.python_version()},
        "settings": {"samples": args.samples, "methods": len(signals), "event": EVENT,
                     "cold": "new process and isolated profile; operating-system caches retained"},
        "results": results,
    }
    validate_result(data)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(args.output), "results": {k: summary(v["samples_ms"]) for k, v in results.items()}}, ensure_ascii=False))


def validate_result(data: object) -> None:
    if not isinstance(data, dict) or data.get("schema") != 2 or data.get("kind") != "coordinator":
        raise ValueError("Неизвестный формат результата; нужны замеры координатора schema=2")
    try:
        settings = data["settings"]
        samples, methods = settings["samples"], settings["methods"]
        if type(samples) is not int or type(methods) is not int or samples < 5 or methods < 1:
            raise ValueError("Недостаточно замеров или неверное число методов")
        if settings["event"] != EVENT or set(data["results"]) != set(LABELS):
            raise ValueError("Неверное событие или неполный набор сценариев")
        hashes = data["inputs"]
        if set(hashes) != {"runtime_sha256", "package_sha256", "harness_sha256"}:
            raise ValueError("Не зафиксированы входы измерителя")
        for value in [*hashes.values(), data["binary_sha256"]]:
            if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
                raise ValueError("Не зафиксированы хэши входов или бинарника")
        for name, result in data["results"].items():
            if (result["verified"] is not True or type(result["warmups"]) is not int
                    or result["warmups"] != WARMUPS[name]
                    or not isinstance(result["samples_ms"], list)
                    or len(result["samples_ms"]) != samples
                    or not isinstance(result["responses"], list) or len(result["responses"]) != samples):
                raise ValueError("Отсутствуют успешные проверки, ответы или сырые замеры")
            for key, expected in summary(result["samples_ms"]).items():
                if type(result[key]) not in (int, float) or result[key] != expected:
                    raise ValueError("Агрегаты не соответствуют сырым замерам")
            for response in result["responses"]:
                if (not isinstance(response["response_sha256"], str)
                        or re.fullmatch(r"[0-9a-f]{64}", response["response_sha256"]) is None
                        or type(response["context_bytes"]) is not int or response["context_bytes"] < 1
                        or type(response["index_entries"]) is not int
                        or response["index_entries"] != (methods if name == "explicit_activation" else 0)):
                    raise ValueError("Не подтверждены контекст и индекс каждого ответа")
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError("Неполный или неверный формат результата") from error


def read_result(path: Path) -> dict:
    data = json.loads(path.read_text(encoding="utf-8"))
    validate_result(data)
    return data


def report(args: argparse.Namespace) -> None:
    data = read_result(args.input)
    print(json.dumps({name: summary(result["samples_ms"]) for name, result in data["results"].items()}, ensure_ascii=False))


def main() -> None:
    parser = argparse.ArgumentParser(description="Сборка и проверяемые замеры координатора fstack без запуска моделей")
    sub = parser.add_subparsers(dest="command", required=True)
    runner = sub.add_parser("run")
    runner.add_argument("--target", help="Cargo target для сборки, по умолчанию текущая платформа")
    runner.add_argument("--output", type=Path, required=True)
    runner.add_argument("--samples", type=int, default=11)
    reporter = sub.add_parser("report")
    reporter.add_argument("input", type=Path)
    args = parser.parse_args()
    if args.command == "run":
        if args.samples < 5:
            parser.error("Нужно не менее пяти замеров")
        run(args)
    else:
        report(args)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
