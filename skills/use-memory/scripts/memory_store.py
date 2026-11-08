from __future__ import annotations

import hashlib
import json
import os
import re
import sqlite3
import stat
import subprocess
import tempfile
from collections.abc import Iterator
from contextlib import closing
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from vault_views import HOME_MARKER, VAULT_FILES, install_views


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
MEMORY_MARKER = "fstack_memory: 1"
APPROVED_MARKER = "fstack_approved: 1"
FORGOTTEN_MARKER = "fstack_forgotten: 1"
FOLDER_MARKERS = {"Records": MEMORY_MARKER, "Approved": APPROVED_MARKER, "Forgotten": FORGOTTEN_MARKER}
TEXT_FIELDS = {"kind": 16, "title": 160, "body": 4000, "source": 300}
METADATA_FIELDS = {"id", "kind", "title", "created", "source", "supersedes", "session_id", "turn_id"}
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


def _directory(path: Path, *, create: bool = False) -> bool:
    for current in (*reversed(path.parents), path):
        info = _info(current)
        if info is None:
            if not create:
                return False
            try:
                current.mkdir(exist_ok=True)
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


def _write_text(path: Path, text: str, *, replace: bool = False) -> bool:
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


def _parse(path: Path, entry_id: str, marker: str) -> dict[str, str] | None:
    raw = _read_text(path)
    if raw is None:
        return None
    if not raw.startswith("---\n"):
        raise StoreError("Invalid memory file marker")
    metadata, end, body = raw[4:].partition("\n---\n")
    lines = metadata.split("\n")
    if not end or lines.count(marker) != 1:
        raise StoreError("Incomplete memory metadata")
    fields = {}
    if marker == MEMORY_MARKER:
        allowed = METADATA_FIELDS
    elif marker == APPROVED_MARKER:
        allowed = {"id", "supersedes"}
    else:
        allowed = {"id"}
    for line in lines:
        if line == marker:
            continue
        field, sep, value = line.partition(": ")
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
        body = _safe_text(body.strip(), 4000, multiline=True)
        _safe_text(fields.get("title"), 160)
        if fields.get("kind") not in KINDS:
            raise StoreError("Invalid memory entry kind")
        for field in fields.keys() - {"id", "title", "kind", "supersedes"}:
            _safe_text(fields[field], 300)
        fields["body"] = body
    elif body.strip():
        raise StoreError("Memory status marker cannot contain text")
    return fields


def _files(directory: Path) -> Iterator[Path]:
    if not _directory(directory):
        return
    try:
        for count, path in enumerate(directory.iterdir(), 1):
            if count > MAX_ENTRIES:
                raise StoreError("Memory directory entry limit exceeded")
            if path.suffix == ".md" and ENTRY_ID.fullmatch(path.stem):
                yield path
    except OSError as error:
        raise StoreError("Cannot list memory entries") from error


class Store:
    def __init__(self, state_dir: Path | None = None):
        if state_dir is None:
            codex_home = Path(os.environ.get("CODEX_HOME") or (Path.home() / ".codex"))
            state_dir = codex_home / "fstack" / "memory"
        self.state_dir = _path(state_dir)

    def _binding_file(self, cwd: Path) -> Path:
        key = hashlib.sha256(_identity(cwd).encode("utf-8", "surrogatepass")).hexdigest()
        return self.state_dir / f"{key}.json"

    def setup(self, vault: Path, repo: Path, project: str, mode: str) -> Binding:
        project, mode = _project(project), _mode(mode)
        settings = self._binding_file(repo)
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
            if not path.is_relative_to(root) or path.relative_to(root).as_posix() not in (
                VIEW_NAMES | {f"Projects/{project}/Project.md"}
            ):
                raise StoreError("Invalid vault view path")
            if path == root / "Home.md" and not self._home_marker(text):
                raise StoreError("Invalid memory home marker")
            if path != root / "Home.md":
                self._check_home(root)
            _directory(path.parent, create=True)
            created = _write_text(path, text)
            self._check_home(root)
            return created

        install_views(root, project, create_text)
        self._check_home(root)
        binding = Binding(project, mode, root)
        for folder in FOLDER_MARKERS:
            _directory(self._project_root(binding) / folder, create=True)
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

    def resolve(self, cwd: Path) -> Binding | None:
        raw = _read_text(self._binding_file(cwd))
        if raw is None:
            return None
        try:
            settings = json.loads(raw)
            root = Path(settings["root"])
            if not root.is_absolute() or root != _path(root):
                raise StoreError("Invalid memory root path")
            binding = Binding(_project(settings["project"]), _mode(settings["mode"]), root)
        except (ValueError, TypeError, KeyError, RecursionError) as error:
            raise StoreError("Invalid memory binding") from error
        self._check_home(root)
        return binding

    def disable(self, cwd: Path) -> bool:
        return _delete(self._binding_file(cwd))

    def _project_root(self, binding: Binding) -> Path:
        self._check_home(binding.root)
        return binding.root / "Projects" / _project(binding.project)

    def _statuses(self, root: Path) -> tuple[dict[str, dict[str, str]], set[str]]:
        _directory(root / "Records")
        hidden = set()
        for path in _files(root / "Forgotten"):
            if _parse(path, path.stem, FORGOTTEN_MARKER) is not None:
                hidden.add(path.stem)
        approvals = {}
        for path in _files(root / "Approved"):
            approval = _parse(path, path.stem, APPROVED_MARKER)
            if approval is not None:
                approvals[path.stem] = approval
                if "supersedes" in approval:
                    hidden.add(approval["supersedes"])
        return approvals, hidden

    @staticmethod
    def _record(root: Path, entry_id: str, approval: dict[str, str] | None) -> dict[str, str] | None:
        filename = f"{entry_id}.md"
        tombstone = root / "Forgotten" / filename
        if _file(tombstone):
            return None
        record = _parse(root / "Records" / filename, entry_id, MEMORY_MARKER)
        if record is None or _file(tombstone):
            return None
        if approval is None:
            return dict(record, status="inbox")
        record.pop("supersedes", None)
        return {**record, **approval, "status": "memory"}

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
            body = fields.pop("body")
            metadata = dict(
                id=entry_id, **fields, created=datetime.now(UTC).isoformat(timespec="seconds"), **provenance,
            )
            _write_text(path, _markdown(MEMORY_MARKER, metadata, body))
            if _file(root / "Forgotten" / path.name):
                _delete(path)
                raise StoreError("Memory entry is unavailable")
        if approved and binding.mode == "auto":
            return self.approve(binding, entry_id)
        approval = _parse(root / "Approved" / path.name, entry_id, APPROVED_MARKER)
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
        root = self._project_root(binding)
        filename = f"{_entry_id(entry_id)}.md"
        approvals, hidden = self._statuses(root)
        if entry_id in hidden:
            raise StoreError("Memory entry is unavailable")
        approval = approvals.get(entry_id)
        record = self._record(root, entry_id, approval)
        if record is None:
            raise StoreError("Inbox entry is unavailable")
        if approval is None:
            self._check_supersedes(root, record, approvals, hidden)
            metadata = {"id": entry_id}
            if "supersedes" in record:
                metadata["supersedes"] = record["supersedes"]
            marker_path = root / "Approved" / filename
            _write_text(marker_path, _markdown(APPROVED_MARKER, metadata))
            approval = _parse(marker_path, entry_id, APPROVED_MARKER)
            record = self._record(root, entry_id, approval)
            if approval is None or record is None:
                raise StoreError("Memory entry is unavailable")
        return record

    def forget(self, binding: Binding, entry_id: str) -> dict:
        root = self._project_root(binding)
        filename = f"{_entry_id(entry_id)}.md"
        tombstone = root / "Forgotten" / filename
        record = root / "Records" / filename
        if not _file(tombstone):
            if not (_file(record) or _file(root / "Approved" / filename) or _file(tombstone)):
                raise StoreError("Memory entry is unavailable")
            _write_text(tombstone, _markdown(FORGOTTEN_MARKER, {"id": entry_id}))
        _delete(record)
        return {"id": entry_id, "status": "forgotten"}
