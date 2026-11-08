from __future__ import annotations

import argparse
import base64
import binascii
import io
import json
import os
import re
import sqlite3
import sys
from pathlib import Path

from hook_spool import (
    MAX_STAGE_BYTES,
    TOKEN,
    PendingRevocationError,
    PendingTurns,
    digest,
    ensure_state_directory,
    read_json,
)
from memory_store import ENTRY_ID, Binding, Store, StoreError, _candidate, _info, _write_text


EVENTS = ("SessionStart", "UserPromptSubmit", "PostToolUse", "Stop")
MAX_HOOK_BYTES = 2_097_152
MAX_PAYLOAD_CHARS = (MAX_STAGE_BYTES * 4 + 2) // 3
PAYLOAD = re.compile(r"[A-Za-z0-9_-]+")
HOOK_ID = re.compile(r"[a-zA-Z0-9_-]{1,128}")
HOOK_ERRORS = (StoreError, OSError, sqlite3.Error, UnicodeError)
RECEIPT = {"staged": True}


def memory_action(value: object) -> dict:
    if isinstance(value, dict):
        action = value.get("action")
        if action in ("remember", "draft") and value.keys() == {"action", "candidate"}:
            return {"action": action, "candidate": _candidate(value["candidate"])}
        if action in ("approve", "forget") and value.keys() == {"action", "id"}:
            entry_id = value["id"]
            if isinstance(entry_id, str) and ENTRY_ID.fullmatch(entry_id):
                return {"action": action, "id": entry_id}
    raise StoreError("Invalid staged memory action")


def encode_action(action: dict) -> str:
    data = json.dumps(action, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(data) > MAX_STAGE_BYTES:
        raise StoreError("Memory action exceeds the size limit")
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def decode_action(payload: str) -> dict:
    if len(payload) > MAX_PAYLOAD_CHARS or not PAYLOAD.fullmatch(payload):
        raise StoreError("Invalid encoded memory action")
    try:
        data = base64.b64decode(payload + "=" * (-len(payload) % 4), altchars=b"-_", validate=True)
    except binascii.Error as error:
        raise StoreError("Invalid encoded memory action") from error
    if base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii") != payload:
        raise StoreError("Invalid encoded memory action")
    return memory_action(read_json(io.BytesIO(data), MAX_STAGE_BYTES))


def _quoted_path(path: Path) -> str:
    text = str(path.resolve())
    if any(char in text for char in '"`$\r\n') or (os.name != "nt" and "\\" in text):
        raise StoreError("Memory command path cannot be used in a shell command")
    return f'"{text}"'


def command_prefix() -> str:
    python = _quoted_path(Path(sys.executable))
    script = _quoted_path(Path(__file__))
    invoke = "& " if os.name == "nt" else ""
    return f"{invoke}{python} -E -s -B {script}"


def posted_stage(payload: dict) -> tuple[str, dict] | None:
    if payload.get("tool_name") != "Bash" or "agent_id" in payload or "agent_type" in payload:
        return None
    call_id = payload.get("tool_use_id")
    if not isinstance(call_id, str) or not HOOK_ID.fullmatch(call_id):
        return None
    tool_input, tool_response = payload.get("tool_input"), payload.get("tool_response")
    if (
        not isinstance(tool_input, dict)
        or set(tool_input) != {"command"}
        or not isinstance(tool_input["command"], str)
        or not isinstance(tool_response, str)
        or tool_response.strip() != json.dumps(RECEIPT)
    ):
        return None
    prefix = re.escape(command_prefix())
    command = re.fullmatch(
        rf"{prefix} stage --turn (?P<token>{TOKEN.pattern}) "
        rf"--payload (?P<encoded>[A-Za-z0-9_-]{{1,{MAX_PAYLOAD_CHARS}}})",
        tool_input["command"],
    )
    if command is None:
        return None
    try:
        return command["token"], decode_action(command["encoded"])
    except StoreError:
        return None


def scope_id(binding: Binding, cwd: Path) -> str:
    return digest(os.path.normcase(str(cwd)), os.path.normcase(str(binding.root)), binding.project, binding.mode)


def extra_context(event: str, context: str) -> dict:
    return {"hookSpecificOutput": {"hookEventName": event, "additionalContext": context}}


def hook(store: Store, event: str, payload: dict) -> dict:
    if payload.get("hook_event_name") != event:
        return {}
    if event == "Stop" and payload.get("stop_hook_active") is not False:
        return {}
    collaboration = payload.get("collaboration_mode")
    if isinstance(collaboration, dict):
        collaboration = collaboration.get("mode")
    plan = "plan" in (payload.get("permission_mode"), collaboration, payload.get("mode"))
    reported_cwd = payload.get("cwd")
    cwd = Path.cwd()
    if not isinstance(reported_cwd, str) or Path(reported_cwd) != cwd:
        return {}
    if event == "SessionStart":
        if plan:
            return {}
        binding = store.resolve(cwd)
        status = f"{binding.project} ({binding.mode})" if binding else "проект не подключён"
        return extra_context(event, f"Память fstack: {status}.")

    session_id, turn_id = payload.get("session_id"), payload.get("turn_id")
    if not all(isinstance(value, str) and HOOK_ID.fullmatch(value) for value in (session_id, turn_id)):
        return {}
    key = digest(session_id, turn_id)
    pending = PendingTurns(store.state_dir, cwd)
    generation = None
    if event == "UserPromptSubmit" or (event == "Stop" and plan):
        try:
            generation = pending.invalidate(key)
        except PendingRevocationError:
            if event == "UserPromptSubmit":
                return {
                    "decision": "block",
                    "reason": "Не удалось отменить предыдущую операцию памяти fstack. Повтори ввод.",
                }
            raise
    if plan:
        return {}
    staged = posted_stage(payload) if event == "PostToolUse" else None
    if event == "PostToolUse" and staged is None:
        return {}
    binding = store.resolve(cwd)
    if binding is None:
        return {}
    scope = scope_id(binding, cwd)
    if event == "UserPromptSubmit":
        prefix = command_prefix()
        prompt = payload.get("prompt")
        ids = []
        if isinstance(prompt, str) and prompt.strip():
            try:
                results = store.search(binding, prompt[:500], limit=3)
                ids = [entry["id"] for entry in results if ENTRY_ID.fullmatch(entry["id"])]
            except HOOK_ERRORS:
                pass
        token = pending.issue(key, scope, generation)
        return extra_context(
            event,
            f"Память fstack, проект {binding.project}, режим {binding.mode}. ID поиска: {', '.join(ids) or '—'}. "
            "Загрузи навык use-memory для правил записи. Перед завершением обычного хода сам оцени, "
            "есть ли обоснованное долговечное решение, поправка или устойчивое предпочтение. "
            "Опирайся на текущую работу, слова пользователя или проверяемый результат. "
            "В режиме auto подготовь такое знание через remember. Разрешение уже даёт режим auto. "
            "Сомнительное, но полезное знание можно направить в draft. "
            "В режиме inbox подготовленные знания поступают на ручную проверку. "
            "Для approve и forget нужно явное поручение пользователя. "
            "В обычном режиме, если есть подходящая запись или явное поручение по памяти, "
            f"передай структурированный JSON в {prefix} pack через stdin. "
            "Затем выполни отдельный точный вызов в оболочке из текущего cwd, подставив ASCII-строку "
            f"из JSON-ответа pack без кавычек вместо BASE64URL: "
            f"{prefix} stage --turn {token} --payload BASE64URL. "
            "В режиме Plan пропусти stage. Не включай токен в ответ.",
        )

    if staged is not None:
        pending.stage(key, staged[0], scope, staged[1])
        return {}
    action_data = pending.consume(key, scope)
    if action_data is None:
        return {}
    try:
        request = memory_action(action_data)
        action = request["action"]
        if action == "approve":
            store.approve(binding, request["id"])
        elif action == "forget":
            store.forget(binding, request["id"])
        else:
            store.save(
                binding,
                request["candidate"],
                approved=action == "remember",
                session_id=session_id,
                turn_id=turn_id,
            )
    except HOOK_ERRORS:
        return {"systemMessage": "Не удалось применить операцию памяти fstack. Повтори поручение в новом ходе."}
    return {}


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(description="Scoped local Obsidian memory")
    cli.add_argument("--state-dir", type=Path)
    commands = cli.add_subparsers(dest="command", required=True)
    init = commands.add_parser("init")
    init.add_argument("--vault", type=Path, required=True)
    init.add_argument("--repo", type=Path, required=True)
    init.add_argument("--project", required=True)
    init.add_argument("--mode", choices=("auto", "inbox"), default="auto")
    commands.add_parser("status")
    commands.add_parser("disable")
    search = commands.add_parser("search")
    search.add_argument("query")
    search.add_argument("--limit", type=int, default=5)
    commands.add_parser("show").add_argument("id")
    commands.add_parser("inbox")
    commands.add_parser("approve").add_argument("id")
    commands.add_parser("forget").add_argument("id")
    commands.add_parser("remember")
    commands.add_parser("pack")
    stage = commands.add_parser("stage")
    stage.add_argument("--turn", required=True)
    stage.add_argument("--payload", required=True)
    commands.add_parser("hook").add_argument("event", choices=EVENTS)
    return cli


def _status(binding: Binding | None) -> dict:
    return {
        "enabled": binding is not None,
        **({"project": binding.project, "mode": binding.mode, "root": str(binding.root)} if binding else {}),
    }


def pinned_python(repo: Path, cwd: Path) -> Path:
    python = Path(sys.executable).resolve(strict=True)
    if (
        python.is_relative_to(repo.resolve())
        or python.is_relative_to(cwd)
        or any(_info(parent / ".git") is not None for parent in python.parents)
        or not python.is_file()
        or not os.access(python, os.X_OK)
        or any(char in str(python) for char in "\r\n")
    ):
        raise StoreError("Initialize memory with a trusted Python installed outside the workspace")
    return python


def command(store: Store, args: argparse.Namespace) -> dict | list[dict] | str:
    cwd = Path.cwd()
    if args.command == "init":
        python = pinned_python(args.repo, cwd)
        _quoted_path(python)
        ensure_state_directory(store.state_dir, args.repo.resolve(), create=True)
        binding = store.setup(args.vault, args.repo, args.project, args.mode)
        _write_text(store.state_dir / "python-executable", f"{python}\n", replace=True)
        return _status(binding)
    if args.command == "disable":
        return {"disabled": store.disable(cwd)}
    if args.command == "pack":
        return encode_action(memory_action(read_json(sys.stdin.buffer, MAX_STAGE_BYTES)))

    binding = store.resolve(cwd)
    if args.command == "status":
        return _status(binding)
    if binding is None:
        raise StoreError("Memory is not enabled for this project")
    if args.command == "search":
        return store.search(binding, args.query, args.limit)
    if args.command == "show":
        return store.get(binding, args.id)
    if args.command == "inbox":
        return store.inbox(binding)
    if args.command == "approve":
        return store.approve(binding, args.id)
    if args.command == "forget":
        return store.forget(binding, args.id)
    if args.command == "remember":
        value = read_json(sys.stdin.buffer, 32_768)
        return store.save(binding, _candidate(value), approved=True)
    if args.command == "stage":
        decode_action(args.payload)
        if not PendingTurns(store.state_dir, cwd).is_current(args.turn, scope_id(binding, cwd)):
            raise StoreError("Invalid or expired memory turn token")
        return RECEIPT
    raise StoreError("Unknown memory command")


def main() -> int:
    args = parser().parse_args()
    if args.command == "hook":
        try:
            result = hook(Store(args.state_dir), args.event, read_json(sys.stdin.buffer, MAX_HOOK_BYTES))
        except HOOK_ERRORS:
            result = {"systemMessage": "Хук памяти fstack не смог обработать событие."}
        print(json.dumps(result))
        return 0
    try:
        print(json.dumps(command(Store(args.state_dir), args)))
    except HOOK_ERRORS as error:
        message = str(error) if isinstance(error, StoreError) else "Local memory operation failed"
        print(message, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
