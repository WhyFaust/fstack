from __future__ import annotations

import errno
import hashlib
import json
import os
import re
import sqlite3
import stat
import subprocess
import tempfile
import time
from collections.abc import Iterator
from contextlib import closing, contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from vault_views import (
    HOME_MARKER, VAULT_FILES, ensure_library, ensure_projects_hub, install_views, is_projects_hub,
    named_project_text, project_template,
)


class StoreError(ValueError):
    pass


@dataclass(frozen=True)
class Binding:
    project: str
    mode: str
    root: Path


ENTRY_ID = re.compile(r"[0-9a-f]{64}")
SLUG = re.compile(r"[a-z0-9][a-z0-9-]{0,47}")
RESERVED_NAMES = {"con", "prn", "aux", "nul", *(f"{prefix}{n}" for prefix in ("com", "lpt") for n in range(10))}
KINDS = {"decision", "preference", "correction"}
SENSITIVE = re.compile(
    r"-----BEGIN [^-\n]*PRIVATE KEY-----|"
    r"\b(?:sk-[\w-]{16,}|github_pat_[\w]{16,}|gh[pousr]_[\w]{16,}|"
    r"AKIA[A-Z0-9]{16}|xox[baprs]-[\w-]{12,}|Bearer\s+[\w./+=-]{8,})\b|"
    r"""\b(?:[\w-]*[_-])?(?:password|passwd|secret|api[_-]?key|(?:access|auth)[_-]?token|token)["']?\s*[:=]\s*["']?[^\s"']{4,}|"""
    r"://[^\s/@]+:[^\s/@]+@",
    re.IGNORECASE,
)
EMBED = re.compile(r"!\[|<\/?[a-zA-Z][^>\n]*>|(?m:^\s*(?:```|~~~))")
TOKENS = re.compile(r"[^\W_]+", re.UNICODE)
STOP_WORDS = frozenset(
    ("a an and are as at be been but by can could did do does for from had has have how i if "
     "in into is it its me my of on or our should that the their this to us was we were what "
     "when where which who why will with would you your "
     "а без был была были было в во вот для до его ее её если есть же за и из или к как "
     "какая какие какой какое ко когда кто ли мне мой мы на над нам наш не ни но о об от "
     "по под при про с со так там то тот у уже что чтобы это этого этой этом этот я").split()
)
MAX_FILE = 32768
MAX_ENTRIES = 4096
GIT_TIMEOUT = 3
LOCK_TIMEOUT = 3
MEMORY_MARKER = "fstack_memory: 1"
APPROVED_MARKER = "fstack_approved: 1"
FORGOTTEN_MARKER = "fstack_forgotten: 1"
TOMBSTONE_SUFFIX = "-forgotten.json"
TEXT_FIELDS = {"kind": 16, "title": 160, "body": 4000, "source": 300}
METADATA_FIELDS = {"id", "kind", "title", "created", "source", "supersedes", "session_id", "turn_id"}
GRAPH_FIELDS = {"fstack_status", "project_link", "supersedes_link"}
GRAPH_LINKS = {
    "project_link": re.compile(
        r"\[\[Codex Memory/Projects/(?P<slug>[a-z0-9][a-z0-9-]{0,47})/(?:Project|(?P=slug))(?:\|(?P=slug))?\]\]"
    ),
    "supersedes_link": re.compile(r"\[\[Codex Memory/Projects/[a-z0-9][a-z0-9-]{0,47}/Records/[0-9a-f]{64}\]\]"),
}
VIEW_NAMES = frozenset(VAULT_FILES)


def _entry_id(value: object) -> str:
    if not isinstance(value, str) or not ENTRY_ID.fullmatch(value):
        raise StoreError("Invalid memory entry ID")
    return value


def _project(value: object) -> str:
    if not isinstance(value, str) or not SLUG.fullmatch(value) or value in RESERVED_NAMES:
        raise StoreError("Invalid memory project slug")
    return value


def _mode(value: object) -> str:
    if value not in ("auto", "inbox"):
        raise StoreError("Invalid memory mode")
    return value


def _safe_text(value: object, limit: int, *, multiline: bool = False) -> str:
    if not isinstance(value, str) or len(value) > limit:
        raise StoreError("Invalid memory text or length")
    value = value.replace("\r\n", "\n").strip()
    if not value or any(
        (ord(char) < 32 or 127 <= ord(char) <= 159 or 0xD800 <= ord(char) <= 0xDFFF)
        and not (multiline and char in "\n\t")
        for char in value
    ):
        raise StoreError("Invalid characters in memory text")
    if SENSITIVE.search(value) or EMBED.search(value):
        raise StoreError("Memory text contains a possible secret, embedded content or a code block")
    return value


def _candidate(candidate: object) -> dict[str, str]:
    if not isinstance(candidate, dict) or candidate.keys() - (TEXT_FIELDS.keys() | {"supersedes"}):
        raise StoreError("Invalid memory candidate fields")
    result = {
        field: _safe_text(candidate.get(field), limit, multiline=field == "body")
        for field, limit in TEXT_FIELDS.items()
        if field != "source" or field in candidate
    }
    if result["kind"] not in KINDS:
        raise StoreError("Invalid memory entry kind")
    if "supersedes" in candidate:
        result["supersedes"] = _entry_id(candidate["supersedes"])
    return result


def _path(path: Path) -> Path:
    return Path(os.path.abspath(path))


def _info(path: Path) -> os.stat_result | None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise StoreError("Cannot inspect memory path") from error
    if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
        raise StoreError("Symbolic links and reparse points are not allowed in memory paths")
    return info


def _directory(path: Path, *, create: bool = False, mode: int = 0o777) -> bool:
    for current in (*reversed(path.parents), path):
        info = _info(current)
        if info is None:
            if not create:
                return False
            try:
                current.mkdir(mode=mode, exist_ok=True)
            except OSError as error:
                raise StoreError("Cannot create memory directory") from error
            info = _info(current)
        if info is None or not stat.S_ISDIR(info.st_mode):
            raise StoreError("Memory path is not a directory")
    return True


def _file(path: Path) -> bool:
    if not _directory(path.parent):
        return False
    info = _info(path)
    if info is not None and not stat.S_ISREG(info.st_mode):
        raise StoreError("Memory path is not a regular file")
    return info is not None


def _read_text(path: Path) -> str | None:
    if not _file(path):
        return None
    try:
        with open(path, "rb", opener=_open_readonly) as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise StoreError("Memory path is not a regular file")
            data = stream.read(MAX_FILE + 1)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise StoreError("Cannot read memory file") from error
    if len(data) > MAX_FILE:
        raise StoreError("Memory file is too large")
    try:
        return data.decode("utf-8-sig").replace("\r\n", "\n")
    except UnicodeError as error:
        raise StoreError("Invalid encoding in memory file") from error


def _open_readonly(path: str, flags: int) -> int:
    return os.open(path, flags | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))


def _write_text(path: Path, text: str, *, replace: bool = False, expected: str | None = None) -> bool:
    if not _directory(path.parent):
        raise StoreError("Missing memory directory")
    if _file(path) and not replace:
        return False
    temp = None
    try:
        with tempfile.NamedTemporaryFile(mode="wb", prefix=".fstack-", dir=path.parent, delete=False) as stream:
            temp = Path(stream.name)
            stream.write(text.encode("utf-8"))
            stream.flush()
            os.fsync(stream.fileno())
        if expected is not None and _read_text(path) != expected:
            raise StoreError("Memory note changed while preparing an update")
        if replace:
            os.replace(temp, path)
        elif os.name == "nt":
            os.rename(temp, path)
        else:
            os.link(temp, path)
        return True
    except FileExistsError:
        _file(path)
        return False
    except OSError as error:
        raise StoreError("Cannot publish memory file atomically") from error
    finally:
        if temp is not None:
            _delete(temp)


def _delete(path: Path) -> bool:
    if not _file(path):
        return False
    try:
        path.unlink()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise StoreError("Cannot remove memory file") from error
    return True


def _lock_directory() -> Path:
    local_data = os.environ.get("LOCALAPPDATA" if os.name == "nt" else "XDG_CACHE_HOME")
    cache = Path(local_data) if local_data else Path.home() / ".cache"
    if not cache.is_absolute() or (os.name == "nt" and cache.drive.startswith("\\\\")):
        raise StoreError("Memory locks require a local absolute cache directory")
    return _path(cache / "fstack" / "memory-locks")


def _try_project_lock(descriptor: int) -> None:
    if os.name == "nt":
        import msvcrt

        msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
    else:
        import fcntl

        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)


@contextmanager
def _mutation_lock(project: Path, vault: Path) -> Iterator[None]:
    directory = _lock_directory()
    if directory.is_relative_to(vault):
        raise StoreError("Local memory lock directory is inside the vault")
    if not _directory(project):
        raise StoreError("Memory project directory is unavailable")
    info = _info(project)
    if info is None or not info.st_ino:
        raise StoreError("Cannot identify the physical memory project")
    key = hashlib.sha256(f"{info.st_dev}:{info.st_ino}".encode("ascii")).hexdigest()
    _directory(directory, create=True, mode=0o700)
    path = directory / f"{key}.lock"
    _file(path)
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except OSError as error:
        raise StoreError("Cannot open the local memory lock") from error
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise StoreError("Local memory lock is not a regular file")
        deadline = time.monotonic() + LOCK_TIMEOUT
        while True:
            try:
                _try_project_lock(descriptor)
                break
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EAGAIN, errno.EDEADLK):
                    raise StoreError("Cannot acquire the local memory lock") from error
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise StoreError("Timed out waiting for the local memory lock") from error
                time.sleep(min(0.025, remaining))
        yield
    finally:
        os.close(descriptor)


def _git_executable(worktree: Path) -> Path:
    name = "git.exe" if os.name == "nt" else "git"
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        directory = Path(entry.strip('"') if os.name == "nt" else entry)
        if not directory.is_absolute() or directory.is_relative_to(worktree):
            continue
        try:
            candidate = (directory / name).resolve(strict=True)
        except (OSError, RuntimeError):
            continue
        if not candidate.is_relative_to(worktree) and candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    raise StoreError("Cannot find Git in an absolute PATH directory outside the worktree")


def _git(executable: Path, cwd: Path, *args: str) -> bytes | None:
    environment = {key: value for key, value in os.environ.items() if not key.upper().startswith("GIT_")}
    environment.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", GIT_OPTIONAL_LOCKS="0")
    try:
        result = subprocess.run(
            [os.fspath(executable), "-C", os.fspath(cwd), *args],
            executable=os.fspath(executable), env=environment,
            capture_output=True, check=False, timeout=GIT_TIMEOUT,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise StoreError("Cannot establish the Git memory scope") from error
    return result.stdout if result.returncode == 0 else None


def _identity(cwd: Path) -> str:
    cwd = _path(cwd)
    if not _directory(cwd):
        raise StoreError("Working directory does not exist")
    git_marker = next(
        (parent for parent in (cwd, *cwd.parents) if _info(parent / ".git") is not None),
        None,
    )
    if git_marker is None:
        return f"dir:{os.path.normcase(cwd)}"
    executable = _git_executable(git_marker)
    result = _git(executable, cwd, "rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir")
    if result is None:
        raise StoreError("Cannot establish the Git memory scope")
    lines = result.splitlines()
    if len(lines) != 2:
        raise StoreError("Invalid Git memory scope")
    root, common = (_path(Path(os.fsdecode(line))) for line in lines)
    if root != git_marker or not _directory(common):
        raise StoreError("Invalid Git memory scope")
    worktrees = _git(executable, cwd, "worktree", "list", "--porcelain", "-z")
    if not worktrees or not any(
        _path(Path(os.fsdecode(field[9:]))) == root
        for field in worktrees.split(b"\0")
        if field.startswith(b"worktree ")
    ):
        raise StoreError("Working directory is not a registered Git worktree")
    return f"git:{os.path.normcase(common)}"


def _markdown(marker: str, fields: dict[str, str], body: str = "") -> str:
    metadata = "\n".join(f"{key}: {json.dumps(value, ensure_ascii=False)}" for key, value in fields.items())
    return f"---\n{marker}\n{metadata}\n---\n\n{body}\n"


def _parse(
    path: Path, entry_id: str, marker: str, *, include_body: bool = True,
) -> dict[str, str] | None:
    raw = _read_text(path)
    if raw is None:
        return None
    return _parse_note(raw, entry_id, marker, include_body=include_body)


def _parse_note(raw: str, entry_id: str, marker: str, *, include_body: bool = True) -> dict[str, str]:
    if not raw.startswith("---\n"):
        raise StoreError("Invalid memory file marker")
    metadata, end, body = raw[4:].partition("\n---\n")
    lines = metadata.split("\n")
    if not end or lines.count(marker) != 1:
        raise StoreError("Incomplete memory metadata")
    fields = {}
    status_only = marker == MEMORY_MARKER and not include_body
    allowed = METADATA_FIELDS | GRAPH_FIELDS if marker == MEMORY_MARKER else {"id", "supersedes"}
    for line in lines:
        if line == marker:
            continue
        field, sep, value = line.partition(": ")
        if status_only and line.partition(":")[0] not in {"id", "fstack_status", "supersedes"}:
            continue
        if not sep or field in fields or field not in allowed:
            raise StoreError("Invalid memory metadata")
        if value.startswith('"'):
            try:
                value = json.loads(value)
            except (ValueError, RecursionError) as error:
                raise StoreError("Invalid memory metadata") from error
        elif value.startswith("'") and value.endswith("'"):
            value = value[1:-1].replace("''", "'")
        if not isinstance(value, str):
            raise StoreError("Invalid memory metadata")
        fields[field] = value
    if fields.get("id") != entry_id:
        raise StoreError("Memory entry ID does not match its file")
    if "supersedes" in fields:
        if _entry_id(fields["supersedes"]) == entry_id:
            raise StoreError("Memory entry cannot supersede itself")
    if marker == MEMORY_MARKER:
        if "fstack_status" in fields and fields["fstack_status"] != "approved":
            raise StoreError("Invalid memory approval status")
        for field, pattern in GRAPH_LINKS.items():
            if field in fields and not pattern.fullmatch(fields[field]):
                raise StoreError("Invalid memory graph link")
        if include_body:
            _safe_text(fields.get("title"), 160)
            if fields.get("kind") not in KINDS:
                raise StoreError("Invalid memory entry kind")
            for field in fields.keys() - GRAPH_FIELDS - {"id", "title", "kind", "supersedes"}:
                _safe_text(fields[field], 300)
            fields["body"] = _safe_text(body.strip(), 4000, multiline=True)
    elif body.strip():
        raise StoreError("Memory status marker cannot contain text")
    return fields


def _project_note(root: Path) -> Path:
    return root / f'{"Project" if root.name == "project" else root.name}.md'


def _is_project_note(raw: str | None, project: str) -> bool:
    if raw is None or not raw.startswith("---\n"):
        return False
    metadata, end, _ = raw[4:].partition("\n---\n")
    markers = [line.partition(":")[2].strip() for line in metadata.splitlines()
               if line.partition(":")[0].rstrip() == "fstack_memory_project"]
    return bool(end and len(markers) == 1 and re.fullmatch(
        rf"""(?:{project}|"{project}"|'{project}')(?:[ \t]+#.*)?""", markers[0],
    ))


def _project_link(page: Path) -> str:
    project = page.parent.name
    return f"[[Codex Memory/Projects/{project}/{page.stem}|{project}]]"


def _current_project_page(root: Path) -> Path | None:
    page = _project_note(root)
    if _is_project_note(_read_text(page), root.name):
        return page
    legacy = root / "Project.md"
    if legacy != page and _is_project_note(_read_text(legacy), root.name):
        return legacy
    return None


def _current_project_link(root: Path) -> str:
    page = _current_project_page(root)
    if page is None:
        raise StoreError("Memory project page is unavailable")
    return _project_link(page)


def _graph_properties(root: Path, supersedes: str | None, project_link: str | None) -> dict[str, str | None]:
    return {
        "project_link": project_link,
        "supersedes_link": f"[[Codex Memory/Projects/{root.name}/Records/{supersedes}]]" if supersedes else None,
    }


def _approval_fields(fields: dict[str, str]) -> dict[str, str]:
    return {key: fields[key] for key in ("id", "supersedes") if key in fields}


def _inline_approval(fields: dict[str, str] | None) -> dict[str, str] | None:
    return _approval_fields(fields) if fields is not None and fields.get("fstack_status") == "approved" else None


def _captured_record(record: Path) -> Path:
    return record.with_name(f".fstack-forget-{record.stem}")


def _record_approval(path: Path, entry_id: str) -> dict[str, str] | None:
    if not _file(path):
        return None
    try:
        return _inline_approval(_parse(path, entry_id, MEMORY_MARKER, include_body=False))
    except StoreError as error:
        if isinstance(error.__cause__, OSError):
            raise
        return None


def _tombstone_paths(root: Path, entry_id: str) -> tuple[Path, Path]:
    return root / "Records" / f"{entry_id}{TOMBSTONE_SUFFIX}", root / "Forgotten" / f"{entry_id}.md"


def _is_forgotten(root: Path, entry_id: str) -> bool:
    canonical, legacy = _tombstone_paths(root, entry_id)
    return _file(legacy) or _file(canonical)


def _parse_tombstone(raw: str, entry_id: str) -> dict[str, str | None]:
    try:
        fields = json.loads(raw)
    except (ValueError, RecursionError) as error:
        raise StoreError("Invalid memory tombstone") from error
    if not isinstance(fields, dict) or fields.keys() - {"id", "supersedes"} or fields.get("id") != entry_id:
        raise StoreError("Invalid memory tombstone")
    predecessor = fields.get("supersedes")
    if predecessor is not None and _entry_id(predecessor) == entry_id:
        raise StoreError("Memory entry cannot supersede itself")
    return fields


def _read_tombstones(
    root: Path, entry_id: str,
) -> tuple[str | None, dict[str, str | None] | None, dict[str, str] | None]:
    canonical, legacy_path = _tombstone_paths(root, entry_id)
    legacy = _parse(legacy_path, entry_id, FORGOTTEN_MARKER)
    raw = _read_text(canonical)
    fields = _parse_tombstone(raw, entry_id) if raw is not None else None
    return raw, fields, legacy


def _tombstone_fields(entry_id: str, approval: dict[str, str] | None) -> dict[str, str | None]:
    if approval is None:
        return {"id": entry_id}
    return {"id": entry_id, "supersedes": approval.get("supersedes")}


def _confirmed_tombstone(
    canonical: dict[str, str | None] | None, legacy: dict[str, str] | None,
) -> dict[str, str | None] | None:
    if canonical is not None and "supersedes" in canonical:
        return canonical
    if legacy is not None and "supersedes" in legacy:
        return _tombstone_fields(legacy["id"], legacy)
    return None


def _write_tombstone(path: Path, raw: str | None, fields: dict[str, str | None]) -> None:
    if not _write_text(path, json.dumps(fields) + "\n", replace=raw is not None, expected=raw):
        raise StoreError("Memory tombstone changed while forgetting an entry")


def _capture_record(record: Path, captured: Path) -> bool:
    if not _file(record):
        return False
    if _file(captured):
        raise StoreError("Previous memory removal is incomplete")
    try:
        os.rename(record, captured)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise StoreError("Cannot capture memory note for removal") from error
    return True


def _refresh_library(root: Path) -> None:
    try:
        ensure_library(root, _read_text, _write_text)
    except (StoreError, OSError):
        pass


def _update_note(path: Path, raw: str, fields: dict[str, str], updates: dict[str, str | None]) -> bool:
    pending = {key: value for key, value in updates.items() if fields.get(key) != value}
    if not pending:
        return False
    metadata, delimiter, body = raw[4:].partition("\n---\n")
    lines = []
    for line in metadata.split("\n"):
        key = line.partition(": ")[0]
        if key not in pending:
            lines.append(line)
        else:
            value = pending.pop(key)
            if value is not None:
                lines.append(f"{key}: {json.dumps(value, ensure_ascii=False)}")
    lines.extend(f"{key}: {json.dumps(value, ensure_ascii=False)}" for key, value in pending.items())
    updated = "---\n" + "\n".join(lines) + delimiter + body
    if len(updated.encode("utf-8")) > MAX_FILE:
        raise StoreError("Updated memory note is too large")
    if not _write_text(path, updated, replace=True, expected=raw):
        raise StoreError("Cannot update memory note")
    return True


def _files(directory: Path, *, tombstones: bool = False) -> Iterator[Path]:
    if not _directory(directory):
        return
    try:
        count = 0
        for path in directory.iterdir():
            is_tombstone = (
                directory.name == "Records" and path.name.endswith(TOMBSTONE_SUFFIX)
                and ENTRY_ID.fullmatch(path.name.removesuffix(TOMBSTONE_SUFFIX)) is not None
            )
            if is_tombstone != tombstones:
                continue
            count += 1
            if count > MAX_ENTRIES:
                if tombstones:
                    raise StoreError("Memory tombstone entry limit exceeded")
                raise StoreError("Memory directory entry limit exceeded")
            if tombstones or (path.suffix == ".md" and ENTRY_ID.fullmatch(path.stem)):
                yield path
    except OSError as error:
        raise StoreError("Cannot list memory entries") from error


def _tombstone_ids(root: Path) -> set[str]:
    ids = {path.stem for path in _files(root / "Forgotten")} | {
        path.name.removesuffix(TOMBSTONE_SUFFIX) for path in _files(root / "Records", tombstones=True)
    }
    if len(ids) > MAX_ENTRIES:
        raise StoreError("Memory tombstone entry limit exceeded")
    return ids


def _remove_empty_directory(path: Path) -> None:
    if not _directory(path):
        return
    try:
        path.rmdir()
    except FileNotFoundError:
        return
    except OSError as error:
        if error.errno not in (errno.ENOTEMPTY, errno.EEXIST) and getattr(error, "winerror", None) != 145:
            raise StoreError("Cannot remove empty legacy memory directory") from error


class Store:
    def __init__(self, state_dir: Path | None = None):
        if state_dir is None:
            codex_home = Path(os.environ.get("CODEX_HOME") or (Path.home() / ".codex"))
            state_dir = codex_home / "fstack" / "memory"
        self.state_dir = _path(state_dir)

    def _binding_file(self, cwd: Path) -> Path:
        key = hashlib.sha256(_identity(cwd).encode("utf-8", "surrogatepass")).hexdigest()
        return self.state_dir / f"{key}.json"

    def setup(self, vault: Path, repo: Path, project: str, mode: str | None) -> Binding:
        project = _project(project)
        settings = self._binding_file(repo)
        if mode is None:
            prior = self._read_binding(settings)
            mode = prior.mode if prior else "auto"
        mode = _mode(mode)
        vault = _path(vault)
        if not _directory(vault / ".obsidian"):
            raise StoreError("Selected directory is not an Obsidian vault")
        root = vault / "Codex Memory"
        _directory(root, create=True)
        home = _read_text(root / "Home.md")
        if home is None:
            try:
                occupied = any(root.iterdir())
            except OSError as error:
                raise StoreError("Cannot inspect memory root") from error
            if occupied:
                home = _read_text(root / "Home.md")
                if not self._home_marker(home):
                    raise StoreError("Memory root is occupied")
        elif not self._home_marker(home):
            raise StoreError("Memory root is occupied")

        def create_text(path: Path, text: str) -> bool:
            path = _path(path)
            if not path.is_relative_to(root) or path.relative_to(root).as_posix() not in VIEW_NAMES:
                raise StoreError("Invalid vault view path")
            if path == root / "Home.md" and not self._home_marker(text):
                raise StoreError("Invalid memory home marker")
            if path != root / "Home.md":
                self._check_home(root)
            _directory(path.parent, create=True)
            created = _write_text(path, text)
            self._check_home(root)
            return created

        install_views(root, create_text)
        self._check_home(root)
        binding = Binding(project, mode, root)
        project_root = self._project_root(binding)
        _directory(project_root, create=True)
        with _mutation_lock(project_root, vault):
            ensure_projects_hub(root, _read_text, _write_text)
            _refresh_library(root)
            if _current_project_page(project_root) is None:
                page = _project_note(project_root)
                if _file(page) or _file(project_root / "Project.md"):
                    raise StoreError("Memory project page is occupied")
                if _directory(project_root / "Records"):
                    raise StoreError("Memory project page is unavailable")
                _write_text(page, project_template(project))
                _current_project_link(project_root)
            _directory(project_root / "Records", create=True)
        _directory(settings.parent, create=True)
        _write_text(settings, json.dumps({"root": str(root), "project": project, "mode": mode}), replace=True)
        return binding

    @staticmethod
    def _home_marker(text: str | None) -> bool:
        if text is None or not text.startswith("---\n"):
            return False
        metadata, delimiter, _ = text[4:].partition("\n---\n")
        return bool(delimiter and HOME_MARKER in metadata.split("\n"))

    def _check_home(self, root: Path) -> None:
        if root.name != "Codex Memory" or not self._home_marker(_read_text(root / "Home.md")):
            raise StoreError("Invalid memory home")

    @staticmethod
    def _read_binding(path: Path) -> Binding | None:
        raw = _read_text(path)
        if raw is None:
            return None
        try:
            settings = json.loads(raw)
            root = Path(settings["root"])
            if root.name != "Codex Memory" or not root.is_absolute() or root != _path(root):
                raise StoreError("Invalid memory root path")
            binding = Binding(_project(settings["project"]), _mode(settings["mode"]), root)
        except (ValueError, TypeError, KeyError, RecursionError) as error:
            raise StoreError("Invalid memory binding") from error
        return binding

    def resolve(self, cwd: Path) -> Binding | None:
        binding = self._read_binding(self._binding_file(cwd))
        if binding is not None:
            self._check_home(binding.root)
        return binding

    def disable(self, cwd: Path) -> bool:
        return _delete(self._binding_file(cwd))

    def _project_root(self, binding: Binding) -> Path:
        self._check_home(binding.root)
        return binding.root / "Projects" / _project(binding.project)

    def _statuses(self, root: Path) -> tuple[dict[str, dict[str, str]], set[str]]:
        approvals = {}
        for path in _files(root / "Approved"):
            approval = _parse(path, path.stem, APPROVED_MARKER)
            if approval is not None:
                approvals[path.stem] = approval
        inline = {}
        unreadable = {}
        for path in _files(root / "Records"):
            if path.stem in approvals:
                continue
            if not _file(path):
                continue
            try:
                raw = _read_text(path)
            except StoreError as error:
                unreadable[path.stem] = error
                continue
            if raw is None:
                continue
            try:
                approval = _inline_approval(_parse_note(raw, path.stem, MEMORY_MARKER, include_body=False))
            except StoreError:
                continue
            if approval is not None:
                inline[path.stem] = approval
        tombstones = {}
        for entry_id in _tombstone_ids(root):
            _, canonical, legacy = _read_tombstones(root, entry_id)
            if canonical is None and legacy is None:
                continue
            confirmed = _confirmed_tombstone(canonical, legacy)
            if confirmed is None and entry_id not in approvals:
                captured = _captured_record(root / "Records" / f"{entry_id}.md")
                approval = _record_approval(captured, entry_id)
                if approval is not None:
                    inline[entry_id] = approval
                _, canonical, legacy = _read_tombstones(root, entry_id)
                confirmed = _confirmed_tombstone(canonical, legacy)
            tombstones[entry_id] = confirmed or {"id": entry_id}
        for entry_id, error in unreadable.items():
            if entry_id not in tombstones:
                raise error
        approvals.update(inline)
        confirmed = approvals | {
            entry_id: fields for entry_id, fields in tombstones.items() if "supersedes" in fields
        }
        superseded = {predecessor for item in confirmed.values() if (predecessor := item.get("supersedes")) is not None}
        return approvals, tombstones.keys() | superseded

    @staticmethod
    def _public_record(record: dict[str, str], approval: dict[str, str] | None) -> dict[str, str]:
        record = {key: value for key, value in record.items() if key not in GRAPH_FIELDS}
        if approval is None:
            return dict(record, status="inbox")
        record.pop("supersedes", None)
        return {**record, **approval, "status": "memory"}

    @staticmethod
    def _record(root: Path, entry_id: str, approval: dict[str, str] | None) -> dict[str, str] | None:
        filename = f"{entry_id}.md"
        if _is_forgotten(root, entry_id):
            return None
        record = _parse(root / "Records" / filename, entry_id, MEMORY_MARKER)
        if record is None or _is_forgotten(root, entry_id):
            return None
        return Store._public_record(record, approval)

    def search(self, binding: Binding, query: str, limit: int = 5) -> list[dict]:
        if not isinstance(query, str) or len(query) > 500 or type(limit) is not int or not 1 <= limit <= 50:
            raise StoreError("Invalid memory search query or limit")
        terms = list(dict.fromkeys(term for term in TOKENS.findall(query.casefold()) if term not in STOP_WORDS))[:20]
        if not terms:
            return []
        root = self._project_root(binding)
        approvals, hidden = self._statuses(root)
        entries = {}
        for entry_id in sorted(approvals.keys() - hidden):
            entry = self._record(root, entry_id, approvals[entry_id])
            if entry is not None:
                entries[entry_id] = entry
        documents = [
            (entry_id, entry["title"].casefold(), entry["body"].casefold())
            for entry_id, entry in entries.items()
        ]
        with closing(sqlite3.connect(":memory:")) as index:
            try:
                index.execute("CREATE VIRTUAL TABLE entries USING fts5(id UNINDEXED, title, body)")
            except sqlite3.OperationalError as error:
                if "no such module: fts5" not in str(error):
                    raise StoreError("Cannot create memory search index") from error
                wanted = set(terms)
                scores = []
                for entry_id, title, body in documents:
                    in_title = wanted.intersection(TOKENS.findall(title))
                    in_body = wanted.intersection(TOKENS.findall(body))
                    if in_title or in_body:
                        scores.append((-len(in_title | in_body), -len(in_title), entry_id))
                return [entries[entry_id] for _, _, entry_id in sorted(scores)[:limit]]
            index.executemany("INSERT INTO entries VALUES (?, ?, ?)", documents)
            expression = " OR ".join(f'"{term}"' for term in terms)
            matches = index.execute(
                "SELECT id FROM entries WHERE entries MATCH ? ORDER BY bm25(entries, 0, 2, 1), id LIMIT ?",
                (expression, limit),
            )
            return [entries[entry_id] for entry_id, in matches]

    def get(self, binding: Binding, entry_id: str) -> dict:
        entry_id = _entry_id(entry_id)
        root = self._project_root(binding)
        approvals, hidden = self._statuses(root)
        if entry_id in approvals and entry_id not in hidden:
            entry = self._record(root, entry_id, approvals[entry_id])
            if entry is not None:
                return entry
        raise StoreError("Memory entry is unavailable")

    def save(
        self, binding: Binding, candidate: dict, *, approved: bool,
        session_id: str | None = None, turn_id: str | None = None,
    ) -> dict:
        fields = _candidate(candidate)
        if type(approved) is not bool:
            raise StoreError("Invalid memory approval")
        root = self._project_root(binding)
        with _mutation_lock(root, binding.root.parent):
            _refresh_library(binding.root)
            return self._save(root, fields, approved and binding.mode == "auto", session_id, turn_id)

    def _save(
        self, root: Path, fields: dict[str, str], approved: bool,
        session_id: str | None, turn_id: str | None,
    ) -> dict:
        project_link = _current_project_link(root)
        identity = {key: value for key, value in fields.items() if key != "source"}
        entry_id = hashlib.sha256(json.dumps(identity, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()
        provenance = {
            field: _safe_text(value, 128)
            for field, value in (("session_id", session_id), ("turn_id", turn_id))
            if value is not None
        }
        approvals, hidden = self._statuses(root)
        if entry_id in hidden:
            raise StoreError("Memory entry is unavailable")
        approval = approvals.get(entry_id)
        path = root / "Records" / f"{entry_id}.md"
        if approval is None and not _file(path):
            self._check_supersedes(root, fields, approvals, hidden)
            metadata = dict(
                id=entry_id, **{key: value for key, value in fields.items() if key != "body"},
                created=datetime.now(UTC).isoformat(timespec="seconds"), **provenance,
            )
            metadata.update(
                (key, value) for key, value in _graph_properties(root, fields.get("supersedes"), project_link).items()
                if value is not None
            )
            if approved:
                metadata["fstack_status"] = "approved"
            if _write_text(path, _markdown(MEMORY_MARKER, metadata, fields["body"])):
                if _is_forgotten(root, entry_id):
                    self._forget(root, entry_id)
                    raise StoreError("Memory entry is unavailable")
                approval = _inline_approval(metadata)
                return self._public_record(dict(metadata, body=fields["body"]), approval)
        if approved:
            return self._approve(root, entry_id)
        approvals, hidden = self._statuses(root)
        if entry_id in hidden:
            raise StoreError("Memory entry is unavailable")
        approval = approvals.get(entry_id)
        record = self._record(root, entry_id, approval)
        if record is None:
            raise StoreError("Memory entry is unavailable")
        return record

    @staticmethod
    def _check_supersedes(root: Path, fields: dict, approvals: dict, hidden: set[str]) -> None:
        predecessor = fields.get("supersedes")
        if predecessor is not None and (
            predecessor not in approvals or predecessor in hidden
            or not _file(root / "Records" / f"{predecessor}.md")
        ):
            raise StoreError("Superseded memory entry is unavailable")

    def inbox(self, binding: Binding) -> list[dict]:
        root = self._project_root(binding)
        approvals, hidden = self._statuses(root)
        result = []
        for path in _files(root / "Records"):
            if path.stem not in approvals and path.stem not in hidden:
                record = self._record(root, path.stem, None)
                if record is not None:
                    result.append(record)
        return sorted(result, key=lambda entry: (entry.get("created", ""), entry["id"]))

    def approve(self, binding: Binding, entry_id: str) -> dict:
        entry_id = _entry_id(entry_id)
        root = self._project_root(binding)
        with _mutation_lock(root, binding.root.parent):
            _refresh_library(binding.root)
            return self._approve(root, entry_id)

    def _approve(self, root: Path, entry_id: str) -> dict:
        project_link = _current_project_link(root)
        filename = f"{entry_id}.md"
        approvals, hidden = self._statuses(root)
        if entry_id in hidden:
            raise StoreError("Memory entry is unavailable")
        approval = approvals.get(entry_id)
        path = root / "Records" / filename
        raw = _read_text(path)
        if raw is None or _is_forgotten(root, entry_id):
            raise StoreError("Inbox entry is unavailable")
        fields = _parse_note(raw, entry_id, MEMORY_MARKER)
        if approval is None:
            self._check_supersedes(root, fields, approvals, hidden)
            approval = _approval_fields(fields)
        predecessor = approval.get("supersedes")
        _update_note(path, raw, fields, {
            "fstack_status": "approved", "supersedes": predecessor,
            **_graph_properties(root, predecessor, project_link),
        })
        if _is_forgotten(root, entry_id):
            raise StoreError("Memory entry is unavailable")
        return self._public_record(fields, approval)

    def forget(self, binding: Binding, entry_id: str) -> dict:
        entry_id = _entry_id(entry_id)
        root = self._project_root(binding)
        with _mutation_lock(root, binding.root.parent):
            return self._forget(root, entry_id)

    def _forget(self, root: Path, entry_id: str) -> dict:
        filename = f"{entry_id}.md"
        tombstone, _ = _tombstone_paths(root, entry_id)
        record = root / "Records" / filename
        captured = _captured_record(record)
        marker = root / "Approved" / filename
        raw, canonical, legacy = _read_tombstones(root, entry_id)
        has_marker = _file(marker)
        has_record = _file(record)
        try:
            approval = _parse(marker, entry_id, APPROVED_MARKER) if has_marker else None
        except StoreError:
            approval = None
        if raw is None:
            if legacy is None and not has_record and not has_marker:
                raise StoreError("Memory entry is unavailable")
            if legacy is None and len(_tombstone_ids(root) | {entry_id}) > MAX_ENTRIES:
                raise StoreError("Memory tombstone entry limit exceeded")
            observed = _confirmed_tombstone(canonical, legacy)
            if observed is None:
                record_approval = approval or _record_approval(captured, entry_id) or _record_approval(record, entry_id)
                observed = _tombstone_fields(entry_id, record_approval)
            _write_tombstone(tombstone, raw, observed)
        self._finish_forget(root, captured, entry_id, approval)
        if _capture_record(record, captured):
            self._finish_forget(root, captured, entry_id, approval)
        return {"id": entry_id, "status": "forgotten"}

    @staticmethod
    def _finish_forget(root: Path, captured: Path, entry_id: str, approval: dict[str, str] | None) -> None:
        raw, canonical, legacy = _read_tombstones(root, entry_id)
        if canonical is None:
            raise StoreError("Memory tombstone disappeared during removal")
        if "supersedes" not in canonical:
            confirmed = _confirmed_tombstone(canonical, legacy) or _tombstone_fields(
                entry_id, approval or _record_approval(captured, entry_id),
            )
            if confirmed != canonical:
                _write_tombstone(_tombstone_paths(root, entry_id)[0], raw, confirmed)
        _delete(captured)

    def migrate(self, binding: Binding, *, prune_legacy: bool = False) -> dict[str, int]:
        root = self._project_root(binding)
        with _mutation_lock(root, binding.root.parent):
            _refresh_library(binding.root)
            if prune_legacy:
                return self._prune_legacy(root)
            ensure_projects_hub(binding.root, _read_text, _write_text)
            return self._migrate(root)

    @staticmethod
    def _migrate(root: Path) -> dict[str, int]:
        updated, skipped = Store._migrate_project_note(root)
        page = _current_project_page(root)
        named = page == _project_note(root)
        project_link = _project_link(page) if page is not None else None
        entry_ids = _tombstone_ids(root) | {
            path.stem for folder in ("Records", "Approved") for path in _files(root / folder)
        }
        for entry_id in sorted(entry_ids):
            filename = f"{entry_id}.md"
            record = root / "Records" / filename
            try:
                approval = _parse(root / "Approved" / filename, entry_id, APPROVED_MARKER)
                tombstone_raw, canonical, legacy = _read_tombstones(root, entry_id)
                if canonical is not None or legacy is not None:
                    confirmed = _confirmed_tombstone(canonical, legacy)
                    if approval is None:
                        approval = _record_approval(_captured_record(record), entry_id) or _record_approval(record, entry_id)
                    desired = confirmed or _tombstone_fields(entry_id, approval)
                    conflict = confirmed is not None and (
                        (legacy is not None and "supersedes" in legacy and confirmed != legacy)
                        or (approval is not None and confirmed != _tombstone_fields(entry_id, approval))
                    )
                else:
                    raw = _read_text(record)
                    if raw is None:
                        skipped += 1
                        continue
                    fields = _parse_note(raw, entry_id, MEMORY_MARKER)
                    predecessor = (approval if approval is not None else fields).get("supersedes")
                    updates = _graph_properties(root, predecessor, project_link)
                    if project_link is None or (not named and "project_link" in fields):
                        del updates["project_link"]
                    if approval is not None:
                        updates.update(fstack_status="approved", supersedes=predecessor)
            except StoreError:
                skipped += 1
                continue
            if canonical is not None or legacy is not None:
                tombstone_changed = desired != canonical
                if tombstone_changed:
                    _write_tombstone(_tombstone_paths(root, entry_id)[0], tombstone_raw, desired)
                record_changed = named and Store._migrate_hidden_project_link(record, project_link)
                updated += tombstone_changed or record_changed
                skipped += conflict and not (tombstone_changed or record_changed)
            else:
                updated += _update_note(record, raw, fields, updates)
        return {"updated": updated, "skipped": skipped}

    @staticmethod
    def _migrate_hidden_project_link(record: Path, project_link: str | None) -> bool:
        try:
            raw = _read_text(record)
            if raw is None:
                return False
            fields = _parse_note(raw, record.stem, MEMORY_MARKER)
        except StoreError:
            return False
        return _update_note(record, raw, fields, {"project_link": project_link})

    @staticmethod
    def _migrate_project_note(root: Path) -> tuple[int, int]:
        legacy_path = root / "Project.md"
        legacy = _read_text(legacy_path)
        page = _project_note(root)
        if legacy is None:
            return 0, int(not _is_project_note(_read_text(page), root.name))
        if not _is_project_note(legacy, root.name):
            return 0, 1
        desired = named_project_text(legacy)
        if len(desired.encode("utf-8")) > MAX_FILE:
            raise StoreError("Updated memory project page is too large")
        if page == legacy_path:
            return int(desired != legacy and _write_text(page, desired, replace=True, expected=legacy)), 0
        if _write_text(page, desired):
            return 1, 0
        return 0, int(_read_text(page) != desired)

    @staticmethod
    def _prune_project_note(root: Path) -> tuple[int, int]:
        legacy_path = root / "Project.md"
        page = _project_note(root)
        if page == legacy_path:
            return 0, 0
        legacy = _read_text(legacy_path)
        if legacy is None:
            return 0, 0
        if not _is_project_note(legacy, root.name) or not is_projects_hub(_read_text(root.parent.parent / "Projects.md")):
            return 0, 1
        desired = named_project_text(legacy)
        if _read_text(page) != desired or any(_files(root / "Approved")):
            return 0, 1
        expected = _project_link(page)
        for record in _files(root / "Records"):
            try:
                fields = _parse(record, record.stem, MEMORY_MARKER)
            except StoreError:
                return 0, 1
            if fields is not None and fields.get("project_link") not in (expected, expected.split("|")[0] + "]]"):
                return 0, 1
        if _read_text(legacy_path) != legacy or _read_text(page) != desired:
            return 0, 1
        return int(_delete(legacy_path)), 0

    @staticmethod
    def _prune_legacy(root: Path) -> dict[str, int]:
        pruned = skipped = 0
        for folder, marker in (("Approved", APPROVED_MARKER), ("Forgotten", FORGOTTEN_MARKER)):
            directory = root / folder
            for path in sorted(_files(directory)):
                try:
                    raw = _read_text(path)
                    if raw is None:
                        continue
                    fields = _parse_note(raw, path.stem, marker)
                    _, canonical, legacy = _read_tombstones(root, path.stem)
                    if marker == FORGOTTEN_MARKER:
                        equivalent = canonical is not None and ("supersedes" not in fields or canonical == fields)
                    elif canonical is not None or legacy is not None:
                        equivalent = canonical == _tombstone_fields(path.stem, fields)
                    else:
                        record = _parse(root / "Records" / path.name, path.stem, MEMORY_MARKER)
                        equivalent = _inline_approval(record) == fields
                    if not equivalent or _read_text(path) != raw:
                        skipped += 1
                        continue
                except StoreError:
                    skipped += 1
                    continue
                pruned += _delete(path)
            _remove_empty_directory(directory)
        removed, retained = Store._prune_project_note(root)
        return {"pruned": pruned + removed, "skipped": skipped + retained}
