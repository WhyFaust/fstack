"""Проверяет маршрут по действиям модели в установленном пакете."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

import app_server
from corpus import snapshot
from evaluate import isolation, temporary_area, write


CASES = {
    "quoted": {
        "prompt": "Что означает запись `$fstack:why` в инструкции?",
        "skills": (),
        "agents": 0,
        "file": None,
        "result": None,
        "no_calls": True,
    },
    "explicit": {
        "prompt": "$fstack:how Объясни, какую очередь выбирает dispatch.py для срочной заявки.",
        "skills": ("how",),
        "agents": 0,
        "file": ("dispatch.py", 'def queue(urgent):\n    return "polar-priority" if urgent else "regular"\n'),
        "result": "polar-priority",
        "explicit_skill": "how",
    },
    "local-review": {
        "prompt": "/faust-mode Прочитай состояние вымышленного GitLab MR из mr.txt и сообщи текущий head. "
                  "Только локальное чтение, без подагентов и внешних вызовов.",
        "skills": ("watch-pr",),
        "agents": 0,
        "file": ("mr.txt", "Текущий head: d7e2c9b08fb90306b7d7e3a94596ea5cb543d141\n"),
        "result": "d7e2c9b08fb90306b7d7e3a94596ea5cb543d141",
        "read_first": "watch-pr",
    },
    "delegate": {
        "prompt": "/faust-mode Поручи одному подагенту на low найти название проекта в README.md. "
                  "Дождись его ответа и назови проект. Работай только на чтение.",
        "skills": ("delegate",),
        "agents": 1,
        "effort": "low",
        "file": ("README.md", "# Каталог керамики «Полярная глина»\n"),
        "result": "Полярная глина",
    },
    "unavailable": {
        "prompt": "/faust-mode Поручи подагенту найти название проекта в README.md. "
                  "Запусти его через провайдера fstack-fixture-unavailable-4481, другие провайдеры не подходят.",
        "skills": ("delegate",),
        "agents": 0,
        "file": ("README.md", "# Полярная глина\n"),
        "result": "fstack-fixture-unavailable-4481",
        "unavailable": True,
    },
}


def grade(case: dict, result: dict, instructions: dict[str, str]) -> list[str]:
    if result["status"] != "completed":
        return [f"Ход не завершён ({result['status']})"]
    routing = result.get("routing")
    if routing is None:
        return ["Клиент не предоставил наблюдения о вызовах и навыках"]
    root = [entry["item"] for entry in result["items"] if entry["threadId"] == result["thread_id"]]
    commands = [(index, (item.get("aggregatedOutput") or "").replace("\r\n", "\n"))
                for index, item in enumerate(root)
                if item["type"] == "commandExecution" and item.get("exitCode") == 0]
    reads = {}
    for index, output in commands:
        for name, source in instructions.items():
            if source and source in output:
                reads.setdefault(name, index)
    issues = []
    if explicit := case.get("explicit_skill"):
        supplied = {name.removeprefix("fstack:") for name in routing["skills_supplied"]}
        if supplied != {explicit} or set(reads) - {explicit}:
            issues.append("Не подтверждена загрузка только явно вызванного навыка")
        if not any(case["result"] in output for _, output in commands):
            issues.append("Не подтверждено чтение исходника")
    else:
        issues.extend(f"Чтение {name} не подтверждено" for name in case["skills"] if name not in reads)
    agents = [entry for entry in app_server.delegations(result)
              if entry.get("kind") == "started" or entry.get("tool") in {"spawnAgent", "spawn_agent"}]
    if len(agents) != case["agents"]:
        issues.append(f"Запущено подагентов {len(agents)}, ожидалось {case['agents']}")
    if "effort" in case and any(agent.get("configured", {}).get("reasoningEffort") != case["effort"]
                                for agent in agents):
        issues.append("Клиент не подтвердил заданную глубину подагента")
    spawns = [call for call in routing["calls"] if call["name"] == "spawn_agent"]
    if "effort" in case:
        if spawns != [{"name": "spawn_agent", "fork_turns": "none", "reasoning_effort": case["effort"]}]:
            issues.append("Не подтверждён единственный запуск в свежем контексте с заданной глубиной")
        if not any(case["result"] in reply for reply in routing["replies"]):
            issues.append("В ходе координатора отсутствует результат подагента")
    if case.get("unavailable"):
        if spawns or any(call["name"] in {"delegate_task", "create_threads", "t3_thread_launch"}
                         for call in routing["calls"]):
            issues.append("Запрошен запуск при недоступном провайдере")
        if not re.search(r"недоступ|не поддерж|не настро|не найден|отсутств|\bнет\b|(?:не позволя(?:ет|ют)|нельзя) выбрать",
                         result["final"], re.I):
            issues.append("Ответ не объясняет недоступность провайдера")
    if case["result"] and case["result"] not in result["final"]:
        issues.append("Ответ не содержит точный результат")
    if case.get("no_calls") and (routing["calls"] or any(item["type"] != "agentMessage" for item in root)):
        issues.append("Контрольный запрос вызвал действие")
    if "read_first" in case:
        skill = case["read_first"]
        first_read = next((index for index, output in commands if case["result"] in output), None)
        if first_read is None or reads.get(skill, len(root)) >= first_read:
            issues.append(f"Чтение {skill} до просмотра снимка не подтверждено")
    return issues


def run_case(args, case: dict) -> dict:
    with temporary_area("routing-") as directory:
        scratch = Path(directory).resolve(strict=True)
        home, workspace = scratch / "profile", scratch / "work"
        workspace.mkdir()
        cache = home / "plugins/cache/fstack"
        app_server.profile(home, workspace, [cache], args.codex, hooks=True)
        if case["file"]:
            name, contents = case["file"]
            (workspace / name).write_text(contents, encoding="utf-8")

        def codex(*command):
            return subprocess.check_output([args.codex, *command], cwd=workspace,
                                           env=app_server.environment(home, isolated_user=True),
                                           encoding="utf-8", stdin=subprocess.DEVNULL, timeout=60)

        codex("plugin", "marketplace", "add", str(args.package))
        installed = Path(json.loads(codex("plugin", "add", "fstack@fstack", "--json"))["installedPath"])
        app_server.trust_fixture_hooks(args.codex, home, workspace)
        preflight = isolation(args.codex, home, workspace, scratch)
        if not preflight["passed"]:
            return {"issues": ["Изоляция не подтвердилась"], "isolation": preflight}
        sources = {path.parent.name: path.read_text(encoding="utf-8").strip()
                   for path in (installed / "skills").glob("*/SKILL.md")}
        before = snapshot(workspace)
        with app_server.authentication(home, args.auth_home):
            result = app_server.run(args.codex, home, workspace, case["prompt"], "",
                                    args.model, args.effort, args.timeout, capture_routing=True)
        issues = grade(case, result, sources)
        if snapshot(workspace) != before:
            issues.append("Рабочие файлы изменились при поручении только на чтение")
        return {"issues": issues, "isolation": preflight, "trace": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=lambda value: Path(value).resolve())
    parser.add_argument("--codex", required=True)
    parser.add_argument("--auth-home", type=Path, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", required=True)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--case", choices=CASES, action="append")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--revision", required=True, help="полный SHA проверяемого пакета")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.revision) or args.repetitions < 1:
        parser.error("укажите полный SHA и положительное число повторов")
    cases = list(dict.fromkeys(args.case or CASES))
    binary = args.package / "bin" / ("fstack.exe" if os.name == "nt" else "fstack")
    with binary.open("rb") as stream:
        binary_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    summary = {"revision": args.revision, "client": subprocess.check_output(
                   [args.codex, "--version"], encoding="utf-8", timeout=10).strip(),
               "runtime_id": (args.package / "launchers/runtime-id").read_text(encoding="ascii").strip(),
               "binary_sha256": binary_sha256,
               "model": args.model, "effort": args.effort, "runs": {}}
    args.output.mkdir(parents=True, exist_ok=False)
    for name in cases:
        for number in range(1, args.repetitions + 1):
            key = f"{name}-{number}"
            result = run_case(args, CASES[name])
            write(args.output / f"{key}.json", result)
            summary["runs"][key] = result["issues"]
            print(f"{key}: " + ("; ".join(result["issues"]) or "pass"), flush=True)
    failed = any(summary["runs"].values())
    summary["status"] = ("failed" if failed else "passed"
                         if set(cases) == CASES.keys() and args.repetitions >= 3 else "partial")
    write(args.output / "summary.json", summary)
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    main()
