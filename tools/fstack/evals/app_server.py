"""Изолированный корневой запуск через Codex App Server (stdio, JSON-RPC)."""
from __future__ import annotations

import contextlib
import json
import os
from pathlib import Path
import queue
import re
import signal
import shutil
import subprocess
import threading
import time


class ClientError(RuntimeError):
    pass


DELEGATION_TYPES = {"collabAgentToolCall", "subAgentActivity"}


def delegations(result: dict) -> list[dict]:
    activities = []
    for entry in result.get("items", []):
        item = entry["item"]
        if item.get("type") not in DELEGATION_TYPES:
            continue
        configured = result.get("agent_threads", {}).get(item.get("agentThreadId"))
        activities.append({**item, **({"configured": configured} if configured else {})})
    return activities


def agent_settings(thread: dict) -> dict:
    return {key: thread[key] for key in ("id", "model", "reasoningEffort") if key in thread}


def environment(home: Path, *, isolated_user: bool = False) -> dict[str, str]:
    # Не передавать исполняемому решению ключи сервисов из окружения разработчика.
    keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP",
            "HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "LANG", "LC_ALL"}
    env = {k: v for k, v in os.environ.items() if k.upper() in keep}
    env.update(CODEX_HOME=str(home), PYTHONIOENCODING="utf-8", NO_COLOR="1")
    if isolated_user:
        user = home.parent / "user"
        user.mkdir(exist_ok=True)
        env.update(HOME=str(user), USERPROFILE=str(user), XDG_CONFIG_HOME=str(user / ".config"),
                   APPDATA=str(user / "AppData/Roaming"), LOCALAPPDATA=str(user / "AppData/Local"))
    return env


def toml_string(value: str | Path) -> str:
    # TOML допускает символы вне BMP напрямую, но не JSON-пары суррогатов.
    return json.dumps(str(value), ensure_ascii=False)


def profile(home: Path, workspace: Path, readonly: list[Path] = (), executable: str | None = None,
            editable: bool = True, *, hooks: bool = False) -> None:
    home.mkdir(parents=True, exist_ok=False)
    scratch = workspace.with_name(workspace.name + "-scratch")
    scratch.mkdir(exist_ok=True)
    if executable:
        binary = Path(executable).resolve()
        # Не разрешать /tmp или /home из-за расположения одного бинарника.
        helpers = [binary, *(binary.with_name(name) for name in
                   ("codex-exec", "codex-exec.exe", "codex-node-repl", "codex-node-repl.exe"))]
        readonly = [*readonly, *(path for path in helpers if path.is_file())]
    roots = "\n".join(f'{toml_string(p.resolve())} = "read"' for p in readonly)
    scratch_path = toml_string(scratch)
    (home / "config.toml").write_text(f'''approval_policy = "never"
default_permissions = "fstack-eval"
web_search = "disabled"
project_doc_max_bytes = 0
cli_auth_credentials_store = "file"

[features]
apps = false
hooks = {str(hooks).lower()}
memory_tool = false
multi_agent = true
multi_agent_v2 = true

[shell_environment_policy]
inherit = "core"

[shell_environment_policy.set]
TMPDIR = {scratch_path}
TMP = {scratch_path}
TEMP = {scratch_path}
XDG_CACHE_HOME = {scratch_path}

[permissions.fstack-eval.filesystem]
":minimal" = "read"
{toml_string(workspace.resolve())} = "{'write' if editable else 'read'}"
{scratch_path} = "write"
# Тесты создают настоящие временные Git-репозитории. Не подставлять fake .git.
{toml_string(scratch / '.git')} = "write"
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
    def __init__(self, executable: str, home: Path, cwd: Path, *, isolated_user: bool = False,
                 path_prefix: Path | None = None, hook_log: Path | None = None):
        env = environment(home, isolated_user=isolated_user)
        if path_prefix is not None:
            env["PATH"] = os.pathsep.join((str(path_prefix), env.get("PATH", "")))
        with contextlib.ExitStack() as resources:
            stderr = subprocess.DEVNULL
            if hook_log is not None:
                env["RUST_LOG"] = "codex_hooks::engine::command_runner=trace"
                stderr = resources.enter_context(hook_log.open("wb"))
            self.process = subprocess.Popen(
                [executable, "app-server", "--listen", "stdio://"], cwd=cwd,
                env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=stderr, text=True, encoding="utf-8", errors="replace",
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


def trust_fixture_hooks(codex: str, home: Path, project: Path) -> dict[str, list[str]]:
    """Доверить точным хешам установленных хуков в одноразовом профиле проверки."""
    server = Server(codex, home, project, isolated_user=True)
    try:
        deadline = time.monotonic() + 45
        server.initialize(deadline)
        groups = server.request("hooks/list", {"cwds": [str(project)]}, deadline)["data"]
    finally:
        server.close()
    hooks = [hook for group in groups for hook in group["hooks"] if hook.get("pluginId") == "fstack@fstack"]
    if not hooks:
        raise ClientError("Codex не обнаружил хуки установленного fstack")
    expected: dict[str, list[str]] = {}
    with (home / "config.toml").open("a", encoding="utf-8") as stream:
        for hook in hooks:
            assert hook["trustStatus"] == "untrusted" and hook["enabled"] is True, hook
            expected.setdefault(hook["eventName"], []).append("untrusted")
            declaration, separator, content = hook["currentHash"].partition("/")
            stream.write(f'\n[hooks.state.{toml_string(hook["key"])}]\ntrusted_hash = {toml_string(declaration)}\n')
            if separator:
                stream.write(f'trusted_content_hash = {toml_string(content)}\n')
    return expected


def observe(result: dict, message: dict) -> None:
    """Сохранять наблюдаемые действия, не reasoning items/дельты рассуждений."""
    method, params = message.get("method"), message.get("params", {})
    if method == "thread/tokenUsage/updated":
        result["usage"][params["threadId"]] = params.get("tokenUsage")
    elif method == "model/rerouted":
        result["reroutes"].append(params)
    elif method == "item/completed":
        item = params.get("item", {})
        if item.get("type") in {"agentMessage", "commandExecution", "fileChange", "mcpToolCall",
                               "dynamicToolCall", "webSearch"} | DELEGATION_TYPES:
            result["items"].append({"threadId": params.get("threadId"), "item": item})
    elif method == "thread/started":
        thread = params.get("thread", {})
        if thread.get("id") and thread["id"] != result.get("thread_id"):
            result.setdefault("agent_threads", {})[thread["id"]] = agent_settings(thread)
    elif method == "error":
        result["errors"].append(params)


def routing_evidence(home: Path, thread_id: str) -> dict:
    if not re.fullmatch(r"[a-f0-9-]{36}", thread_id):
        raise ClientError("Некорректный ID журнала проверки маршрута")
    logs = list((home / "sessions").glob(f"*/*/*/rollout-*-{thread_id}.jsonl"))
    if len(logs) != 1:
        raise ClientError("Не найден единственный журнал проверки маршрута")
    calls, skills, replies = [], [], []
    wait_calls = set()
    with logs[0].open(encoding="utf-8") as stream:
        for line in stream:
            row = json.loads(line)
            if row["type"] != "response_item":
                continue
            item = row["payload"]
            if item["type"] in {"function_call", "custom_tool_call"}:
                call = {"name": item["name"]}
                if item["name"] == "spawn_agent":
                    arguments = json.loads(item["arguments"])
                    call.update({key: arguments.get(key) for key in ("fork_turns", "reasoning_effort")})
                if item["name"] == "wait_agent":
                    wait_calls.add(item["call_id"])
                calls.append(call)
            elif item["type"] == "agent_message" and item.get("recipient") == "/root":
                replies.append(" ".join(part.get("text", "") for part in item["content"]))
            elif item["type"] == "function_call_output" and item["call_id"] in wait_calls:
                output = item["output"]
                replies.append(output if isinstance(output, str) else json.dumps(output, ensure_ascii=False))
            elif item["type"] == "message" and item.get("role") == "user":
                for part in item.get("content", []):
                    match = re.match(r"<skill>\s*<name>([^<]+)</name>", part.get("text", "").lstrip())
                    if match:
                        skills.append(match.group(1))
    return {"calls": calls, "skills_supplied": skills, "replies": replies}


def run(executable: str, home: Path, workspace: Path, prompt: str, instructions: str,
        model: str, effort: str, timeout: int, *, capture_routing: bool = False) -> dict:
    start = time.monotonic()
    deadline = start + timeout
    result = {"status": "environment_error", "requested_model": model, "requested_effort": effort,
              "usage": {}, "reroutes": [], "items": [], "errors": [], "final": ""}
    server = Server(executable, home, workspace)
    thread_id = turn_id = None
    try:
        result["client"] = server.initialize(deadline)
        started = server.request("thread/start", {
            "cwd": str(workspace), "ephemeral": not capture_routing, "model": model,
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
        agent_ids = {x["item"]["agentThreadId"] for x in result["items"]
                     if x["item"]["type"] == "subAgentActivity" and x["item"].get("kind") == "started"}
        for agent_id in sorted(agent_ids):
            if result.get("agent_threads", {}).get(agent_id, {}).get("model"):
                continue
            try:
                child = server.request("thread/read", {"threadId": agent_id, "includeTurns": False}, deadline)
                result.setdefault("agent_threads", {})[agent_id] = agent_settings(child["thread"])
            except (ClientError, TimeoutError, KeyError) as exc:
                result.setdefault("agent_metadata_errors", {})[agent_id] = str(exc)
        if capture_routing and result["status"] == "completed":
            result["routing"] = routing_evidence(home, thread_id)
    except TimeoutError as exc:
        result.update(status="timeout", error=str(exc))
        if thread_id and turn_id:
            with contextlib.suppress(Exception):
                server.request("turn/interrupt", {"threadId": thread_id, "turnId": turn_id}, time.monotonic() + 5)
    except (ClientError, OSError, KeyError, ValueError) as exc:
        result["error"] = str(exc)
        result["status"] = "environment_error"
    finally:
        server.close()
        result["elapsed_seconds"] = round(time.monotonic() - start, 3)
    return result
