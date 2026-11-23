"""Проверяет маршрут по действиям модели в установленном пакете."""
import argparse
import json
from pathlib import Path
import subprocess

import app_server
from corpus import snapshot
from evaluate import isolation, temporary_area, write


CASES = {
    "quoted": {
        "prompt": "Что означает запись `$fstack:why` в инструкции? Ответь без чтения файлов и подагентов.",
        "skills": (),
        "agents": 0,
        "file": None,
        "result": None,
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
}


def grade(case: dict, result: dict, instructions: dict[str, str]) -> list[str]:
    if result["status"] != "completed":
        return [f"Ход не завершён ({result['status']})"]
    root = [entry["item"] for entry in result["items"] if entry["threadId"] == result["thread_id"]]
    commands = [(index, (item.get("aggregatedOutput") or "").replace("\r\n", "\n"))
                for index, item in enumerate(root)
                if item["type"] == "commandExecution" and item.get("exitCode") == 0]
    reads = {}
    for index, output in commands:
        for name in case["skills"]:
            if instructions.get(name) and instructions[name] in output:
                reads.setdefault(name, index)
    issues = [f"Чтение {name} не подтверждено" for name in case["skills"] if name not in reads]
    agents = [entry for entry in app_server.delegations(result)
              if entry.get("kind") == "started" or entry.get("tool") in {"spawnAgent", "spawn_agent"}]
    if len(agents) != case["agents"]:
        issues.append(f"Запущено подагентов {len(agents)}, ожидалось {case['agents']}")
    if "effort" in case and any(agent.get("configured", {}).get("reasoningEffort") != case["effort"]
                                for agent in agents):
        issues.append("Клиент не подтвердил заданную глубину подагента")
    if case["result"] and case["result"] not in result["final"]:
        issues.append("Ответ не содержит точный результат")
    if case["file"] is None and any(item["type"] == "commandExecution" for item in root):
        issues.append("Контрольный запрос вызвал команду")
    if "read_first" in case:
        skill = case["read_first"]
        first_read = next((index for index, output in commands if case["result"] in output), None)
        if first_read is None or reads.get(skill, len(root)) >= first_read:
            issues.append(f"Чтение {skill} до просмотра снимка не подтверждено")
    return issues


def run_case(args, case: dict) -> dict:
    with temporary_area("routing-") as directory:
        scratch = Path(directory)
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
                                    args.model, args.effort, args.timeout)
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
    parser.add_argument("--case", choices=CASES, action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    failed = False
    for name in dict.fromkeys(args.case):
        result = run_case(args, CASES[name])
        failed |= bool(result["issues"])
        write(args.output / f"{name}.json", result)
        print(f"{name}: " + ("; ".join(result["issues"]) or "pass"))
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
