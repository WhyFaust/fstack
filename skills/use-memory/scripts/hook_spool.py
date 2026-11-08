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
from pathlib import Path
from typing import BinaryIO

from memory_store import StoreError, _directory, _info


MAX_STAGE_BYTES = 20_000
TTL_SECONDS = 3600
TURN_LIMIT = 256
TOKEN = re.compile(r"([0-9a-f]{64})\.([0-9a-f]{32})")


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


class PendingTurns:
    def __init__(self, state_dir: Path, cwd: Path):
        self.state_dir = state_dir
        self.cwd = cwd
        self.path = state_dir / "pending.sqlite3"

    def _available(self, *, create: bool = False) -> bool:
        if self.state_dir.is_relative_to(self.cwd) or any(
            _info(parent / ".git") is not None for parent in (self.state_dir, *self.state_dir.parents)
        ):
            raise StoreError("Memory hook state must be outside the workspace")
        if not _directory(self.state_dir, create=create):
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

    def invalidate(self, key: str) -> None:
        with self._write() as connection:
            if connection is not None:
                connection.execute("DELETE FROM pending WHERE turn=? OR expires<?", (key, int(time.time())))

    def issue(self, key: str, scope: str) -> str:
        now = int(time.time())
        nonce = secrets.token_hex(16)
        with self._write(create=True) as connection:
            if connection is None:
                raise StoreError("Cannot open pending memory database")
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
        if match is None or not self._available():
            return False
        uri = f"{self.path.as_uri()}?mode=ro"
        with closing(sqlite3.connect(uri, timeout=1, uri=True)) as connection:
            return connection.execute(
                "SELECT 1 FROM pending WHERE turn=? AND nonce=? AND scope=? AND expires>=?",
                (*match.groups(), scope, int(time.time())),
            ).fetchone() is not None

    def stage(self, key: str, token: str, scope: str, action: dict) -> None:
        match = TOKEN.fullmatch(token)
        if match is None or match[1] != key:
            return
        serialized = json.dumps(action, ensure_ascii=False, separators=(",", ":"))
        with self._write() as connection:
            if connection is not None:
                connection.execute(
                    "UPDATE pending SET action=? WHERE turn=? AND nonce=? AND scope=? "
                    "AND expires>=? AND (action IS NULL OR action=?)",
                    (serialized, key, match[2], scope, int(time.time()), serialized),
                )

    @contextmanager
    def consume(self, key: str, scope: str) -> Iterator[object]:
        with self._write() as connection:
            if connection is None:
                yield None
                return
            row = connection.execute(
                "SELECT action FROM pending WHERE turn=? AND scope=? AND expires>=?",
                (key, scope, int(time.time())),
            ).fetchone()
            yield json.loads(row[0]) if row and row[0] else None
            connection.execute("DELETE FROM pending WHERE turn=?", (key,))
