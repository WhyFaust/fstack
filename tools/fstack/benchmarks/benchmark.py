import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
REPORT = ROOT / "docs/benchmarks.md"
RESULT = ROOT / "docs/benchmarks/runtime-windows.json"
START = "<!-- benchmark:runtime:start -->"
END = "<!-- benchmark:runtime:end -->"
LABELS = {
    "show": "Прочитать известное решение",
    "search_hit": "Найти известное решение",
    "search_miss": "Проверить отсутствие решения",
}


def fingerprint(paths, root=ROOT):
    digest = hashlib.sha256()
    for path in sorted(paths, key=lambda p: p.relative_to(root).as_posix()):
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def inputs():
    runtime = ROOT / "tools/fstack"
    templates = ROOT / "skills/use-memory/assets/vault"
    if not (templates / "Home.md").is_file():
        raise ValueError("Нет обязательного шаблона памяти Home.md")
    return {
        "runtime_sha256": fingerprint([
            *runtime.glob("src/**/*.rs"), runtime / "Cargo.toml", runtime / "Cargo.lock",
        ]),
        "templates_sha256": fingerprint([
            path for path in templates.rglob("*") if path.is_file()
        ]),
        "harness_sha256": fingerprint([Path(__file__).resolve()]),
    }


def summary(samples):
    if not samples or any(not math.isfinite(v) or v < 0 for v in samples):
        raise ValueError("Замеры должны быть непустыми, конечными и неотрицательными")
    return {
        "median_ms": round(statistics.median(samples), 3),
        "p95_ms": round(sorted(samples)[math.ceil(len(samples) * 0.95) - 1], 3),
    }


def expect_search(value, expected_id):
    expected = [] if expected_id is None else [expected_id]
    if not isinstance(value, list) or [item.get("id") for item in value] != expected:
        raise ValueError("Поиск вернул неверные записи; такой замер не считается успешным")


def measure(operation, samples):
    operation()
    timings = []
    for _ in range(samples):
        started = time.perf_counter_ns()
        operation()
        timings.append(round((time.perf_counter_ns() - started) / 1_000_000, 3))
    return {"samples_ms": timings, **summary(timings), "verified": True}


def build(target):
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


def run(args):
    before = inputs()
    binary = build(args.target)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="fstack-benchmark-") as temporary:
        temporary = Path(temporary).resolve()
        project, vault, state = [temporary / name for name in ("project", "vault", "state")]
        for folder in (project, vault, state, temporary / "user"):
            folder.mkdir()
        (vault / ".obsidian").mkdir()
        env = dict(os.environ)
        for name, folder in {
            "CODEX_HOME": "codex", "HOME": "user", "USERPROFILE": "user",
            "APPDATA": "config", "LOCALAPPDATA": "cache",
            "XDG_CONFIG_HOME": "config", "XDG_CACHE_HOME": "cache",
        }.items():
            env[name] = str(temporary / folder)

        def memory(arguments, payload=None):
            result = subprocess.run(
                [str(binary), "--package", str(ROOT), "memory", "--state-dir", str(state),
                 *map(str, arguments)],
                input=None if payload is None else json.dumps(payload).encode(),
                capture_output=True, cwd=project, env=env, timeout=120,
            )
            if result.returncode:
                raise RuntimeError(result.stderr.decode("utf-8", errors="replace"))
            return json.loads(result.stdout)

        memory(["init", "--vault", vault, "--repo", project, "--project", "benchmark", "--mode", "auto"])
        target = None
        target_body = None
        for index in range(args.records):
            body = f"Решение для компонента {index}: использовать его штатный интерфейс."
            if index == args.records // 2:
                body += " Кварцевит обозначает нужное решение."
            saved = memory(["remember"], {
                "kind": "decision", "title": f"Компонент {index}", "body": body,
                "source": "Синтетический открытый корпус benchmark.py",
            })
            if index == args.records // 2:
                target = saved["id"]
                target_body = body
        if not target:
            raise ValueError("Контрольная запись не создана")

        def search(query, expected):
            value = memory(["search", query, "--limit", "5"])
            expect_search(value, expected)
            if expected and value[0].get("body") != target_body:
                raise ValueError("Поиск потерял содержимое записи")

        def show():
            value = memory(["show", target])
            expect_search([value], target)
            if value.get("body") != target_body:
                raise ValueError("Чтение потеряло содержимое записи")

        results = {
            "show": measure(show, args.samples),
            "search_hit": measure(lambda: search("Кварцевит", target), args.samples),
            "search_miss": measure(lambda: search("Гелиотроп", None), args.samples),
        }
    if inputs() != before:
        raise ValueError("Исходники изменились во время замера; повторите запуск")
    if hashlib.sha256(binary.read_bytes()).hexdigest() != binary_hash:
        raise ValueError("Бинарник изменился во время замера; повторите запуск")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT))
    data = {
        "schema": 1, "kind": "runtime", "created_at": datetime.now(timezone.utc).isoformat(),
        "revision": revision, "tracked_changes": dirty, "inputs": before,
        "binary_sha256": binary_hash,
        "build": {"toolchain": "1.90.0", "profile": "release", "locked": True,
                  "target": args.target, "verified_by": "cargo compiler-artifact from measured sources"},
        "environment": {"system": platform.system(), "release": platform.release(),
                        "machine": platform.machine(), "processor": platform.processor(),
                        "python": platform.python_version()},
        "settings": {"samples": args.samples, "warmups": 1, "records": args.records},
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(args.output), "results": {k: summary(v["samples_ms"]) for k, v in results.items()}}, ensure_ascii=False))


def read_result(path):
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema") != 1 or data.get("kind") != "runtime":
        raise ValueError("Неизвестный формат результата")
    if data["settings"]["samples"] < 5 or data["settings"]["records"] < 2:
        raise ValueError("Недостаточно замеров или записей")
    if set(data["results"]) != set(LABELS):
        raise ValueError("Неполный набор сценариев")
    for result in data["results"].values():
        if result["verified"] is not True or len(result["samples_ms"]) != data["settings"]["samples"]:
            raise ValueError("Отсутствуют успешные проверки или сырые замеры")
        if any(result[k] != v for k, v in summary(result["samples_ms"]).items()):
            raise ValueError("Агрегаты не соответствуют сырым замерам")
    return data


def render(data):
    current = data["inputs"] == inputs()
    state = "соответствуют текущим измеряемым файлам" if current else "устарели: изменились измеряемые файлы, нужен новый запуск"
    lines = [START, "", f"Замер {data['created_at'][:10]} · {data['environment']['system']} {data['environment']['machine']} · данные {state}.", "",
             f"{data['settings']['samples']} повторов после одного прогрева; {data['settings']['records']} записей памяти.", "",
             "| Сценарий | Медиана, мс | p95, мс | Проверка результата |",
             "| --- | ---: | ---: | --- |"]
    for name, result in data["results"].items():
        lines.append(f"| {LABELS[name]} | {result['median_ms']:.3f} | {result['p95_ms']:.3f} | Пройдена |")
    lines += ["", "[Все замеры и параметры](benchmarks/runtime-windows.json).", "", END]
    return "\n".join(lines)


def report(args):
    data = read_result(RESULT)
    text = REPORT.read_text(encoding="utf-8")
    start, end = text.index(START), text.index(END) + len(END)
    expected = text[:start] + render(data) + text[end:]
    if args.check:
        if text != expected:
            raise ValueError("Обновите таблицу и статус: python tools/fstack/benchmarks/benchmark.py report")
        print("Таблица, сырые замеры и статус актуальности согласованы")
    else:
        REPORT.write_text(expected, encoding="utf-8", newline="\n")


def main():
    parser = argparse.ArgumentParser(description="Сборка и воспроизводимые замеры памяти fstack без запуска моделей")
    sub = parser.add_subparsers(dest="command", required=True)
    runner = sub.add_parser("run")
    runner.add_argument("--target", help="Cargo target для сборки, по умолчанию текущая платформа")
    runner.add_argument("--output", type=Path, required=True)
    runner.add_argument("--samples", type=int, default=11)
    runner.add_argument("--records", type=int, default=256)
    reporter = sub.add_parser("report")
    reporter.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if args.command == "run":
        if args.samples < 5 or args.records < 2:
            parser.error("Нужно не менее пяти замеров и двух записей")
        run(args)
    else:
        report(args)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    main()
