from __future__ import annotations

import hashlib
import json
import os
import re
import secrets
import sqlite3
import stat
import time
from collections.abc import Iterator
from contextlib import closing, contextmanager
from itertools import islice
from pathlib import Path
from typing import BinaryIO

from memory_store import StoreError, _delete, _directory, _info, _read_text, _write_text


MAX_STAGE_BYTES = 20_000
TTL_SECONDS = 3600
TURN_LIMIT = 256
TOKEN = re.compile(r"([0-9a-f]{64})\.([0-9a-f]{32})")
TURN_KEY = re.compile(r"[0-9a-f]{64}")
LOCAL_MODE = 0o777 if os.name == "nt" else 0o700
REVOCATION_LIMIT = 4 * TURN_LIMIT


class PendingRevocationError(StoreError):
    pass


class PendingActionConflict(StoreError):
    pass


def _unique_fields(items: list[tuple[str, object]]) -> dict[str, object]:
    fields = dict(items)
    if len(fields) != len(items):
        raise ValueError("Duplicate JSON fields")
    return fields


def read_json(stream: BinaryIO, limit: int) -> dict:
    data = stream.read(limit + 1)
    if len(data) > limit:
        raise StoreError("Memory input exceeds the size limit")
    try:
        value = json.loads(data.decode("utf-8-sig"), object_pairs_hook=_unique_fields)
    except (ValueError, RecursionError) as error:
        raise StoreError("Invalid UTF-8 JSON memory input") from error
    if not isinstance(value, dict):
        raise StoreError("Expected a JSON object")
    return value


def digest(*parts: str) -> str:
    data = json.dumps(parts, ensure_ascii=True).encode("ascii")
    return hashlib.sha256(data).hexdigest()


def ensure_state_directory(state_dir: Path, cwd: Path, *, create: bool = False) -> bool:
    if state_dir.is_relative_to(cwd) or any(
        _info(parent / ".git") is not None for parent in (state_dir, *state_dir.parents)
    ):
        raise StoreError("Memory hook state must be outside the workspace")
    return _directory(state_dir, create=create, mode=LOCAL_MODE)


class PendingTurns:
    def __init__(self, state_dir: Path, cwd: Path):
        self.state_dir = state_dir
        self.cwd = cwd
        self.path = state_dir / "pending.sqlite3"
        self.generations = state_dir / "pending-generations"
        self.revocations = state_dir / "pending-revocations"

    def _state_available(self, *, create: bool = False) -> bool:
        return ensure_state_directory(self.state_dir, self.cwd, create=create)

    def _available(self, *, create: bool = False) -> bool:
        if not self._state_available(create=create):
            return False
        if create and _info(self.path) is None:
            try:
                descriptor = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            except FileExistsError:
                pass
            else:
                os.close(descriptor)
        info = _info(self.path)
        if info is None:
            return False
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or (os.name != "nt" and (info.st_uid != os.getuid() or info.st_mode & 0o077))
        ):
            raise StoreError("Unsafe pending memory database")
        journal = _info(self.path.with_suffix(".sqlite3-journal"))
        if journal is not None and (not stat.S_ISREG(journal.st_mode) or journal.st_nlink != 1):
            raise StoreError("Unsafe pending memory journal")
        if any(_info(self.path.with_suffix(suffix)) is not None for suffix in (".sqlite3-wal", ".sqlite3-shm")):
            raise StoreError("Unexpected pending memory database files")
        return True

    @contextmanager
    def _write(self, *, create: bool = False) -> Iterator[sqlite3.Connection | None]:
        if not self._available(create=create):
            yield None
            return
        uri = f"{self.path.as_uri()}?mode=rw"
        with closing(sqlite3.connect(uri, timeout=1, uri=True)) as connection, connection:
            connection.execute("PRAGMA secure_delete=ON")
            connection.execute("BEGIN IMMEDIATE")
            connection.execute(
                "CREATE TABLE IF NOT EXISTS pending ("
                "turn TEXT PRIMARY KEY, nonce TEXT NOT NULL, scope TEXT NOT NULL, "
                "expires INTEGER NOT NULL, action TEXT)"
            )
            yield connection

    def _generation(self, key: str) -> str | None:
        if not self._state_available():
            return None
        return _read_text(self.generations / key)

    def _revoked(self, key: str, nonce: str) -> bool:
        name = f"{key}.{nonce}"
        if not TOKEN.fullmatch(name):
            raise StoreError("Invalid pending memory generation")
        return _directory(self.revocations) and _info(self.revocations / name) is not None

    def _current(self, key: str, nonce: str) -> bool:
        return self._generation(key) == nonce and not self._revoked(key, nonce)

    def invalidate(self, key: str) -> str | None:
        if not self._state_available():
            return None
        try:
            previous = _read_text(self.generations / key)
            if previous is None:
                return None
            if not self._revoked(key, previous):
                _directory(self.revocations, create=True, mode=LOCAL_MODE)
                self._prune(self.revocations, TOKEN, REVOCATION_LIMIT)
                _write_text(self.revocations / f"{key}.{previous}", "")
                if not self._revoked(key, previous):
                    raise StoreError("Missing pending memory revocation marker")
        except (StoreError, OSError) as error:
            raise PendingRevocationError("Cannot revoke pending memory action") from error
        nonce = secrets.token_hex(16)
        _write_text(self.generations / key, nonce, replace=True)
        return nonce

    @staticmethod
    def _prune(directory: Path, pattern: re.Pattern[str], limit: int) -> None:
        now = time.time()
        with os.scandir(directory) as entries:
            paths = list(islice(entries, limit + 1))
        remaining = len(paths)
        for entry in paths:
            if not pattern.fullmatch(entry.name):
                continue
            try:
                if (
                    entry.is_file(follow_symlinks=False)
                    and now - entry.stat(follow_symlinks=False).st_mtime > TTL_SECONDS + 1
                ):
                    if _delete(Path(entry.path)):
                        remaining -= 1
            except (FileNotFoundError, PermissionError):
                continue
        if remaining >= limit:
            raise StoreError("Pending memory marker limit exceeded")

    def _new_generation(self, key: str) -> str:
        self._state_available(create=True)
        _directory(self.generations, create=True, mode=LOCAL_MODE)
        self._prune(self.generations, TURN_KEY, TURN_LIMIT)
        nonce = secrets.token_hex(16)
        if not _write_text(self.generations / key, nonce):
            raise StoreError("Memory turn was replaced by a newer input")
        return nonce

    def issue(self, key: str, scope: str, generation: str | None) -> str:
        now = int(time.time())
        nonce = generation if generation is not None else self._new_generation(key)
        with self._write(create=True) as connection:
            if connection is None:
                raise StoreError("Cannot open pending memory database")
            if not self._current(key, nonce):
                raise StoreError("Memory turn was replaced by a newer input")
            connection.execute("DELETE FROM pending WHERE expires<?", (now,))
            if connection.execute("SELECT count(*) FROM pending").fetchone()[0] >= TURN_LIMIT:
                raise StoreError("Pending memory turn limit exceeded")
            connection.execute(
                "INSERT INTO pending (turn, nonce, scope, expires) VALUES (?, ?, ?, ?) "
                "ON CONFLICT(turn) DO UPDATE SET nonce=excluded.nonce, scope=excluded.scope, "
                "expires=excluded.expires, action=NULL",
                (key, nonce, scope, now + TTL_SECONDS),
            )
        return f"{key}.{nonce}"

    def is_current(self, token: str, scope: str) -> bool:
        match = TOKEN.fullmatch(token)
        if match is None or not self._current(*match.groups()) or not self._available():
            return False
        uri = f"{self.path.as_uri()}?mode=ro"
        with closing(sqlite3.connect(uri, timeout=1, uri=True)) as connection:
            current = connection.execute(
                "SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=?",
                (*match.groups(), scope, int(time.time())),
            ).fetchone() is not None
        return current and self._current(*match.groups())

    def stage(self, key: str, token: str, scope: str, action: dict) -> None:
        match = TOKEN.fullmatch(token)
        if match is None or match[1] != key or not self._current(key, match[2]):
            return
        serialized = json.dumps(action, ensure_ascii=False, separators=(",", ":"))
        with self._write() as connection:
            if connection is None or not self._current(key, match[2]):
                return
            identity = (key, match[2], scope, int(time.time()))
            result = connection.execute(
                "UPDATE pending SET action=? WHERE turn=? AND nonce=? AND scope=? "
                "AND expires>=? AND (action IS NULL OR action=?)",
                (serialized, *identity, serialized),
            )
            if result.rowcount:
                return
            existing = connection.execute(
                "SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=? AND action<>?",
                (*identity, serialized),
            ).fetchone()
            if existing and self._current(key, match[2]):
                raise PendingActionConflict("Another memory action is already pending for this turn")

    def consume(self, key: str, scope: str) -> object:
        nonce = self._generation(key)
        if nonce is None:
            return None
        with self._write() as connection:
            if connection is None or self._generation(key) != nonce:
                return None
            row = None
            if self._current(key, nonce):
                row = connection.execute(
                    "SELECT action FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=?",
                    (key, nonce, scope, int(time.time())),
                ).fetchone()
            connection.execute("DELETE FROM pending WHERE turn=?", (key,))
        if row and row[0] and self._current(key, nonce):
            return json.loads(row[0])
        return None
