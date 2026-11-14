"""Изолированный корневой запуск через Codex App Server (stdio, JSON-RPC)."""
from __future__ import annotations

import contextlib
import json
import os
from pathlib import Path
import queue
import signal
import shutil
import subprocess
import threading
import time


class ClientError(RuntimeError):
    pass


def environment(home: Path) -> dict[str, str]:
    # Не передавать исполняемому решению ключи сервисов из окружения разработчика.
    keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP",
            "HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "LANG", "LC_ALL"}
    env = {k: v for k, v in os.environ.items() if k.upper() in keep}
    env.update(CODEX_HOME=str(home), PYTHONIOENCODING="utf-8", NO_COLOR="1")
    return env


def profile(home: Path, workspace: Path, readonly: list[Path] = (), executable: str | None = None,
            editable: bool = True) -> None:
    home.mkdir(parents=True, exist_ok=False)
    scratch = workspace.with_name(workspace.name + "-scratch")
    scratch.mkdir(exist_ok=True)
    if executable:
        binary = Path(executable).resolve()
        # Не разрешать /tmp или /home из-за расположения одного бинарника.
        helpers = [binary, *(binary.with_name(name) for name in
                   ("codex-exec", "codex-exec.exe", "codex-node-repl", "codex-node-repl.exe"))]
        readonly = [*readonly, *(path for path in helpers if path.is_file())]
    roots = "\n".join(f'{json.dumps(str(p.resolve()))} = "read"' for p in readonly)
    (home / "config.toml").write_text(f'''approval_policy = "never"
default_permissions = "fstack-eval"
web_search = "disabled"
project_doc_max_bytes = 0
cli_auth_credentials_store = "file"

[features]
apps = false
memory_tool = false
multi_agent = true

[shell_environment_policy]
inherit = "core"

[shell_environment_policy.set]
TMPDIR = {json.dumps(str(scratch))}
TMP = {json.dumps(str(scratch))}
TEMP = {json.dumps(str(scratch))}
XDG_CACHE_HOME = {json.dumps(str(scratch))}

[permissions.fstack-eval.filesystem]
":minimal" = "read"
{json.dumps(str(workspace.resolve()))} = {json.dumps('write' if editable else 'read')}
{json.dumps(str(scratch))} = "write"
# Тесты создают настоящие временные Git-репозитории. Не подставлять fake .git.
{json.dumps(str(scratch / '.git'))} = "write"
{roots}

[permissions.fstack-eval.network]
enabled = false
''', encoding="utf-8")


@contextlib.contextmanager
def authentication(home: Path, auth_home: Path):
    """Только существующая файловая авторизация CLI, без пользовательских настроек."""
    source = auth_home / "auth.json"
    if not source.is_file():
        raise ClientError("Нужна файловая авторизация отдельного Codex CLI профиля; выполните codex login.")
    target = home / "auth.json"
    try:
        shutil.copyfile(source, target)
        target.chmod(0o600)
        yield
    finally:
        target.unlink(missing_ok=True)


class Server:
    def __init__(self, executable: str, home: Path, cwd: Path):
        self.process = subprocess.Popen(
            [executable, "app-server", "--listen", "stdio://"], cwd=cwd,
            env=environment(home), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, encoding="utf-8", errors="replace",
            start_new_session=os.name != "nt",
        )
        self.messages: queue.Queue = queue.Queue()
        self.pending: list[dict] = []
        self.serial = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            for line in self.process.stdout:
                try:
                    self.messages.put(json.loads(line))
                except json.JSONDecodeError:
                    self.messages.put({"protocol_error": "App Server вернул не JSON"})
        finally:
            self.messages.put({"protocol_error": "App Server завершился"})

    def send(self, value: dict):
        self.process.stdin.write(json.dumps(value, ensure_ascii=False) + "\n")
        self.process.stdin.flush()

    def receive(self, deadline: float) -> dict:
        if time.monotonic() >= deadline:
            raise TimeoutError("Истекло время ожидания App Server")
        try:
            message = self.messages.get(timeout=max(0.001, deadline - time.monotonic()))
        except queue.Empty as exc:
            raise TimeoutError("Истекло время ожидания App Server") from exc
        if "protocol_error" in message:
            raise ClientError(message["protocol_error"])
        if "method" in message and "id" in message:
            # Никаких интерактивных approvals, ввода или динамических инструментов.
            self.send({"id": message["id"], "error": {"code": -32601, "message": "Unavailable in evaluation"}})
            raise ClientError("Запуск запросил интерактивное действие: " + message["method"])
        return message

    def request(self, method: str, params: dict, deadline: float):
        self.serial += 1
        serial = self.serial
        self.send({"id": serial, "method": method, "params": params})
        while True:
            message = self.receive(deadline)
            if message.get("id") == serial:
                if "error" in message:
                    raise ClientError(json.dumps(message["error"], ensure_ascii=False))
                return message["result"]
            self.pending.append(message)

    def initialize(self, deadline: float):
        result = self.request("initialize", {
            "clientInfo": {"name": "fstack_evaluation", "version": "1.0.0"},
            "capabilities": {"experimentalApi": True},
        }, deadline)
        self.send({"method": "initialized", "params": {}})
        return result

    def close(self):
        if self.process.poll() is None:
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                               capture_output=True, timeout=10, check=False)
            else:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if os.name != "nt":
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(self.process.pid, signal.SIGKILL)
                else:
                    self.process.kill()
                self.process.wait(timeout=5)
        for stream in (self.process.stdin, self.process.stdout):
            if stream:
                stream.close()


def observe(result: dict, message: dict) -> None:
    """Сохранять наблюдаемые действия, не reasoning items/дельты рассуждений."""
    method, params = message.get("method"), message.get("params", {})
    if method == "thread/tokenUsage/updated":
        result["usage"][params["threadId"]] = params.get("tokenUsage")
    elif method == "model/rerouted":
        result["reroutes"].append(params)
    elif method == "item/completed":
        item = params.get("item", {})
        if item.get("type") in {"agentMessage", "commandExecution", "fileChange", "collabAgentToolCall"}:
            result["items"].append({"threadId": params.get("threadId"), "item": item})
    elif method == "error":
        result["errors"].append(params)


def run(executable: str, home: Path, workspace: Path, prompt: str, instructions: str,
        model: str, effort: str, timeout: int) -> dict:
    start = time.monotonic()
    deadline = start + timeout
    result = {"status": "environment_error", "requested_model": model, "requested_effort": effort,
              "usage": {}, "reroutes": [], "items": [], "errors": [], "final": ""}
    server = Server(executable, home, workspace)
    thread_id = turn_id = None
    try:
        result["client"] = server.initialize(deadline)
        started = server.request("thread/start", {
            "cwd": str(workspace), "ephemeral": True, "model": model,
            "approvalPolicy": "never", "permissions": "fstack-eval",
            "allowProviderModelFallback": False,
            "config": {"model_reasoning_effort": effort},
            "developerInstructions": instructions,
        }, deadline)
        result["resolved"] = {k: v for k, v in started.items() if k != "thread"}
        thread_id = started["thread"]["id"]
        result["thread_id"] = thread_id
        if started.get("model") != model or started.get("reasoningEffort") != effort:
            raise ClientError("Фактическая модель/reasoning не совпадает с запрошенной парой")
        active = started.get("activePermissionProfile") or {}
        if (started.get("approvalPolicy") != "never" or active.get("id") != "fstack-eval"
                or Path(started.get("cwd", "")).resolve() != workspace.resolve()
                or started.get("sandbox", {}).get("type") in {"dangerFullAccess", "externalSandbox"}):
            raise ClientError("Клиент не подтвердил профиль разрешений fstack-eval")
        turn = server.request("turn/start", {"threadId": thread_id,
                              "model": model, "effort": effort,
                              "input": [{"type": "text", "text": prompt}]}, deadline)
        turn_id = turn["turn"]["id"]
        while True:
            if time.monotonic() >= deadline:
                raise TimeoutError("Истекло время выполнения поручения")
            message = server.pending.pop(0) if server.pending else server.receive(deadline)
            observe(result, message)
            params = message.get("params", {})
            if message.get("method") == "turn/completed" and params.get("threadId") == thread_id:
                result["turn"] = {k: v for k, v in params["turn"].items() if k != "items"}
                result["status"] = "completed" if params["turn"]["status"] == "completed" else "environment_error"
                break
        if any(r.get("threadId") == thread_id and r.get("turnId") == turn_id for r in result["reroutes"]):
            result["status"] = "model_mismatch"
        messages = [x["item"] for x in result["items"] if x["threadId"] == thread_id
                    and x["item"]["type"] == "agentMessage"]
        finals = [m for m in messages if m.get("phase") == "final_answer"]
        finals = finals or [m for m in messages if m.get("phase") is None]
        result["final"] = finals[-1]["text"] if finals else ""
    except TimeoutError as exc:
        result.update(status="timeout", error=str(exc))
        if thread_id and turn_id:
            with contextlib.suppress(Exception):
                server.request("turn/interrupt", {"threadId": thread_id, "turnId": turn_id}, time.monotonic() + 5)
    except (ClientError, OSError, KeyError) as exc:
        result["error"] = str(exc)
    finally:
        server.close()
        result["elapsed_seconds"] = round(time.monotonic() - start, 3)
    return result
