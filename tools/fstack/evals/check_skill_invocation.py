"""Проверяет обнаружение и явную загрузку всех установленных навыков Codex."""
import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from app_server import Server, environment, trust_fixture_hooks


def check_mr_route(instruction: str, coordinator: Path, watch_pr: Path) -> None:
    route = re.search(
        r"До первого чтения или записи по GitLab MR[^.]*?открой\s+"
        r"\[\$fstack:watch-pr\]\(([^)]+)\)", instruction,
    )
    if not route or (coordinator.parent / route[1]).resolve() != watch_pr:
        raise RuntimeError("В инструкции координатора нет перехода к установленному watch-pr до первого обращения к MR")


def check_quote_guard(name: str, instruction: str) -> None:
    guard = "Цитата не включает навык."
    if guard not in " ".join(instruction.split()):
        raise RuntimeError(f"В переданном тексте {name} нет правила для цитирования")


class ResponsesHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.inputs.append(request["input"])
        sequence = uuid.uuid4().hex
        message = {"id": f"msg_{sequence}", "type": "message", "role": "assistant", "status": "completed",
                   "content": [{"type": "output_text", "text": "OK", "annotations": []}]}
        response = {"id": f"resp_{sequence}", "object": "response", "status": "completed", "output": [message]}
        events = [
            {"type": "response.created", "response": {**response, "status": "in_progress", "output": []}},
            {"type": "response.output_item.done", "output_index": 0, "item": message},
            {"type": "response.completed", "response": response},
        ]
        body = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args) -> None:
        pass


def start_thread(server: Server, workspace: Path, *, ephemeral: bool = True) -> str:
    started = server.request("thread/start", {
        "cwd": str(workspace), "ephemeral": ephemeral, "modelProvider": "fixture",
        "approvalPolicy": "never", "sandbox": "read-only",
    }, time.monotonic() + 60)
    return started["thread"]["id"]


def turn_texts(server: Server, provider: ThreadingHTTPServer, thread_id: str, prompt: str,
               *, effort: str | None = None) -> list[str]:
    provider.inputs.clear()
    deadline = time.monotonic() + 60
    params = {"threadId": thread_id, "input": [{"type": "text", "text": prompt}]}
    if effort is not None:
        params["effort"] = effort
    server.request("turn/start", params, deadline)
    while True:
        msg = server.pending.pop(0) if server.pending else server.receive(deadline)
        if msg.get("method") == "hook/completed" and msg["params"]["threadId"] == thread_id:
            hook = msg["params"]["run"]
            if hook["status"] == "failed":
                raise RuntimeError(
                    f"Хук {hook['eventName']} завершился ошибкой для {prompt!r} "
                    f"за {hook.get('durationMs')} мс: {hook.get('entries') or hook.get('statusMessage')}"
                )
        if msg.get("method") == "turn/completed" and msg["params"]["threadId"] == thread_id:
            if msg["params"]["turn"]["status"] != "completed":
                raise RuntimeError(f"Локальный запрос не завершён: {msg['params']['turn'].get('error')}")
            break
    if len(provider.inputs) != 1:
        raise RuntimeError(f"Ожидался один локальный запрос, получено {len(provider.inputs)}")
    texts = []
    for item in provider.inputs[0]:
        if item.get("role") == "assistant":
            # При повторной отправке истории оцениваем только ввод после предыдущего ответа заглушки.
            texts.clear()
        else:
            texts.extend(part["text"].replace("\r\n", "\n") for part in item.get("content", [])
                         if part.get("type") == "input_text")
    if prompt not in texts:
        raise RuntimeError(f"В локальном запросе не найден ввод {prompt!r}")
    return texts


def catalog_signals(root: Path, entries: dict[str, Path]) -> dict[str, str]:
    signals = {}
    for line in (root / "docs/skills.md").read_text(encoding="utf-8").splitlines():
        row = re.fullmatch(r"\| \[([a-z0-9-]+)\]\(\.\./skills/\1/SKILL\.md\) \| (.+?) \|", line)
        if row:
            signals[row[1]] = row[2].split(", [English](")[0]
    if {f"fstack:{name}" for name in signals} != entries.keys():
        raise RuntimeError("Сигналы в установленном каталоге не совпадают со списком навыков")
    return signals


def check_hook_context(texts: list[str], root: Path, signals: dict[str, str], *, active: bool, index: bool):
    context = "\n".join(texts)
    mode = "В этом чате включён faust-mode" if active else "Faust-mode выключен."
    if mode not in context:
        raise RuntimeError(f"В контексте текущего запроса отсутствует сообщение хука {mode!r}")
    if not active and "В этом чате включён faust-mode" in context:
        raise RuntimeError("Обычный запрос получил приглашение активного координатора")
    if active and (str(root / "skills/route/SKILL.md") not in context or str(root / "docs/skills.md") not in context):
        raise RuntimeError("Активный хук не передал установленные пути маршрутизатора и каталога")
    for name, signal in signals.items():
        count = context.count(f"{name} — {signal}")
        if count != int(index):
            raise RuntimeError(f"Сигнал {name!r} встречается {count} раз, ожидалось {int(index)}")


def cold_marketplace(package: Path, scratch: Path, *, stub: bool) -> tuple[Path, Path | None]:
    market = scratch / "marketplace"
    shutil.copytree(package, market, ignore=shutil.ignore_patterns("bin", ".git", "target"))
    if not stub:
        return market, None
    transport = scratch / "transport"
    transport.mkdir()
    binary = transport / "binary"
    shutil.copy2(package / "bin/fstack", binary)
    (transport / "sha256").write_text(hashlib.sha256(binary.read_bytes()).hexdigest(), encoding="ascii")
    curl = transport / "curl"
    curl.write_text(r"""#!/bin/sh
set -eu
root=${0%/*}
while [ "${1#https://}" = "$1" ]; do shift; done
case "$1" in
    *.sha256)
        name=${1##*/}
        printf '%s  %s\n' "$(cat "$root/sha256")" "${name%.sha256}" > "$3"
        ;;
    *) sleep 8; cp "$root/binary" "$3" ;;
esac
""", encoding="utf-8")
    curl.chmod(0o700)
    return market, transport


def windows_hook_probe(package: Path, scratch: Path) -> Path:
    market = scratch / "marketplace"
    shutil.copytree(package, market, ignore=shutil.ignore_patterns(".git", "target"))
    record = "[IO.File]::AppendAllText([IO.Path]::Combine($env:CODEX_HOME,'windows-hook.log'),"
    wrapper = market / "hooks/coordinator.ps1"
    code = wrapper.read_text(encoding="utf-8")
    for marker, label in (("$ErrorActionPreference = 'Stop'", "script"),
                          ("$binary = & $launch $operation", "launcher"),
                          ("$process = [Diagnostics.Process]::Start($start)", "spawn"),
                          ("    $process.WaitForExit()", "wait"),
                          ("    exit $process.ExitCode", "exit")):
        if code.count(marker) != 1:
            raise RuntimeError(f"Точка наблюдения Windows не найдена: {marker}")
        code = code.replace(marker, f"{record} $EventName+': {label}'+[Environment]::NewLine)\n{marker}")
    wrapper.write_text(code, encoding="utf-8")
    return market


def check(package: Path, codex: str, scratch: Path, *, measure_cold: bool = False) -> dict | None:
    home = scratch / "profile"
    workspace = scratch / "workspace"
    home.mkdir()
    workspace.mkdir()
    env = environment(home, isolated_user=True)
    cold_profile = measure_cold or os.name != "nt"
    marketplace, transport = (cold_marketplace(package, scratch, stub=not measure_cold)
                              if cold_profile else (windows_hook_probe(package, scratch), None))

    def run(*args: str) -> str:
        result = subprocess.run([codex, *args], cwd=workspace, env=env,
                                stdin=subprocess.DEVNULL, capture_output=True, encoding="utf-8", timeout=120)
        if result.returncode:
            raise RuntimeError(f"codex {args[0]}: {result.stderr.strip()}")
        return result.stdout

    with ThreadingHTTPServer(("127.0.0.1", 0), ResponsesHandler) as provider:
        provider.inputs = []
        (home / "config.toml").write_text(f'''model = "gpt-6.2-sol"
model_provider = "fixture"
check_for_update_on_startup = false
[features]
apps = false
hooks = true
remote_plugin = false
[model_providers.fixture]
name = "Local fixture"
base_url = "http://127.0.0.1:{provider.server_port}/v1"
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
''', encoding="utf-8")
        run("plugin", "marketplace", "add", str(marketplace))
        installed = json.loads(run("plugin", "add", "fstack@fstack", "--json"))
        root = Path(installed["installedPath"]).resolve()
        sources = {f"fstack:{entry.parent.name}": entry.read_text(encoding="utf-8").replace("\r\n", "\n").strip()
                   for entry in sorted((package / "skills").glob("*/SKILL.md"))}
        entries = {f"fstack:{entry.parent.name}": entry.resolve()
                   for entry in (root / "skills").glob("*/SKILL.md")}
        if not sources or sources.keys() != entries.keys():
            raise RuntimeError("Набор установленных навыков отличается от пакета")
        for name, source in sources.items():
            if entries[name].read_text(encoding="utf-8").replace("\r\n", "\n").strip() != source:
                raise RuntimeError(f"Установленная инструкция {name} отличается от пакета")

        binary = root / "bin" / ("fstack.exe" if os.name == "nt" else "fstack")
        if cold_profile and (binary.exists() or (home / "fstack/runtime").exists()):
            raise RuntimeError("Для проверки первого запроса нужен пустой кэш")
        if not cold_profile and (binary.is_symlink() or not binary.is_file() or not os.access(binary, os.X_OK)):
            raise RuntimeError(f"Снимок установлен без исполняемого runtime {binary}")
        signals = catalog_signals(root, entries)
        trust_fixture_hooks(codex, home, workspace)
        hook_log = scratch / "hooks.log"
        server = Server(codex, home, workspace, isolated_user=True, path_prefix=transport, hook_log=hook_log)
        thread = threading.Thread(target=provider.serve_forever)
        thread.start()
        try:
            server.initialize(time.monotonic() + 60)
            if cold_profile:
                opening = time.monotonic()
                cold = start_thread(server, workspace)
                opened = time.monotonic()
                texts = turn_texts(server, provider, cold, "/faust-mode")
                ready = time.monotonic()
                check_hook_context(texts, root, signals, active=True, index=True)
                texts = turn_texts(server, provider, cold, "Продолжи первый чат.")
                check_hook_context(texts, root, signals, active=True, index=False)
                if measure_cold:
                    return {"open_seconds": opened - opening, "ready_seconds": ready - opening,
                            "package_version": installed["version"], "client": run("--version").strip(),
                            "runtime_id": (root / "launchers/runtime-id").read_text(encoding="ascii").strip()}
            groups = server.request("skills/list", {"cwds": [str(workspace)], "forceReload": True},
                                    time.monotonic() + 60)["data"]
            if not groups or any(group.get("errors") for group in groups):
                raise RuntimeError(f"Ошибки обнаружения навыков: {groups}")
            discovered = [skill for group in groups for skill in group["skills"]
                          if skill.get("pluginId") == "fstack@fstack"]
            if (len(discovered) != len(entries) or {skill["name"] for skill in discovered} != entries.keys()
                    or any(skill.get("enabled") is not True
                           or Path(skill["path"]).resolve() != entries[skill["name"]] for skill in discovered)):
                raise RuntimeError("Codex не обнаружил все включённые навыки по установленным путям")
            improvement = "Изучи весь проект и предложи, какие улучшения взять в работу."
            cases = [("Проверь изменение.", None),
                     (improvement, None),
                     (f"$fstack:audit {improvement}", "fstack:audit"),
                     ("Объясни разницу между why, how и test-update в каталоге навыков.", None)]
            manifest = json.loads((root / ".codex-plugin/plugin.json").read_text(encoding="utf-8"))
            route_prompt = next((prompt for prompt in manifest["interface"]["defaultPrompt"]
                                 if prompt.startswith("$fstack:route ")), None)
            if route_prompt is None:
                raise RuntimeError("В карточке плагина нет прямого вызова route")
            cases.append((route_prompt, "fstack:route"))
            cases.extend((f"${name} Проверь доступность инструкции.", name) for name in sources
                         if name not in {"fstack:audit", "fstack:faust-mode", "fstack:why", "fstack:route"})
            cases.append(("$fstack:why По истории проекта выясни причину решения.", "fstack:why"))
            cases.append(("$fstack:faust-mode Проверь состояние существующего GitLab MR.", "fstack:faust-mode"))
            for prompt, selected in cases:
                texts = turn_texts(server, provider, start_thread(server, workspace), prompt)
                loaded = {name for name, source in sources.items() if any(source in text for text in texts)}
                expected = {selected} if selected is not None else set()
                if loaded != expected:
                    raise RuntimeError(
                        f"Неверная загрузка SKILL.md для {prompt!r}. "
                        f"Ожидалось {sorted(expected)}, получено {sorted(loaded)}."
                    )
                if selected is not None:
                    check_quote_guard(selected, sources[selected])
                if selected == "fstack:faust-mode":
                    instruction = next(text for text in texts if sources[selected] in text)
                    check_mr_route(instruction, entries[selected], entries["fstack:watch-pr"])
                else:
                    try:
                        check_hook_context(texts, root, signals, active=False, index=False)
                    except RuntimeError as exc:
                        raise RuntimeError(f"Запрос {prompt!r}: {exc}") from exc

            quoted = "Объясни пример команды в документации: `$fstack:why`."
            texts = turn_texts(server, provider, start_thread(server, workspace), quoted)
            check_hook_context(texts, root, signals, active=False, index=False)
            for name, source in sources.items():
                if any(source in text for text in texts):
                    check_quote_guard(name, source)

            thread_id = start_thread(server, workspace)
            texts = turn_texts(server, provider, thread_id, "/faust-mode Разберись, почему выбрали этот формат.")
            check_hook_context(texts, root, signals, active=True, index=True)
            texts = turn_texts(server, provider, start_thread(server, workspace), "Новый вопрос по проекту.")
            check_hook_context(texts, root, signals, active=False, index=False)
            for prompt, active in [
                ("Теперь проверь обновлённую инструкцию.", True),
                ("/faust-mode-off", False),
                ("Продолжи обычный разговор.", False),
            ]:
                texts = turn_texts(server, provider, thread_id, prompt)
                check_hook_context(texts, root, signals, active=active, index=False)

            thread_id = start_thread(server, workspace, ephemeral=False)
            for prompt, effort, active, index in [
                ("Начни разбор.", "ultra", True, True),
                ("Продолжи разбор.", "ultra", True, True),
                ("Уточни детали.", "high", False, False),
                ("Вернись к разбору.", "ultra", True, True),
                ("/faust-mode-off", "ultra", False, False),
                ("Продолжи после выключения.", "ultra", False, False),
                ("/faust-mode", "high", True, True),
                ("Продолжи после ручного включения.", "high", True, False),
            ]:
                texts = turn_texts(server, provider, thread_id, prompt, effort=effort)
                check_hook_context(texts, root, signals, active=active, index=index)

            neighbor = start_thread(server, workspace, ephemeral=False)
            texts = turn_texts(server, provider, neighbor, "Начни отдельный разбор.", effort="high")
            check_hook_context(texts, root, signals, active=False, index=False)
            texts = turn_texts(server, provider, neighbor, "Продолжи отдельный разбор.", effort="ultra")
            check_hook_context(texts, root, signals, active=True, index=True)

            texts = turn_texts(server, provider, start_thread(server, workspace), "Проверь временный чат.", effort="ultra")
            check_hook_context(texts, root, signals, active=False, index=False)
        except (RuntimeError, TimeoutError):
            phases = home / "windows-hook.log"
            if os.name == "nt":
                if phases.is_file():
                    print("\n".join(phases.read_text(encoding="utf-8").splitlines()[-20:]), file=sys.stderr)
                else:
                    print("Windows: код PowerShell не записал ни одной фазы запуска", file=sys.stderr)
            outcomes = [line for line in hook_log.read_text(encoding="utf-8", errors="replace").splitlines()
                        if "hook.command_outcome" in line]
            for outcome in outcomes[-12:]:
                print(outcome, file=sys.stderr)
            raise
        finally:
            server.close()
            provider.shutdown()
            thread.join()
    print(f"{run('--version').strip()}: проверены прямые вызовы всех {len(sources)} навыков, "
          "обычный запрос, разбор имён и цитаты, индекс, ручное переключение, выбор глубины в постоянном и временном чате.")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--cold-samples", type=int, metavar="N", help="измерить N запусков с реальной загрузкой")
    args = parser.parse_args()
    if args.cold_samples is not None and args.cold_samples < 20:
        parser.error("для p95 нужно не менее 20 запусков")
    samples = []
    for _ in range(args.cold_samples or 1):
        with tempfile.TemporaryDirectory(prefix="fstack-skill-check-") as directory:
            sample = check(args.package.resolve(), args.codex, Path(directory).resolve(strict=True),
                           measure_cold=args.cold_samples is not None)
            if sample is not None:
                samples.append(sample)
    if samples:
        readiness = sorted(sample["ready_seconds"] for sample in samples)
        p95 = readiness[math.ceil(len(readiness) * .95) - 1]
        longest = max(sample["open_seconds"] for sample in samples)
        print(json.dumps({"samples": samples, "ready_p95_seconds": p95, "open_max_seconds": longest}))
        sys.exit(0 if p95 <= 10 and longest <= 15 else 1)
