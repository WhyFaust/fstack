from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import TextIO


@contextmanager
def open_journal(directory: Path, *, keep_lock: bool = False) -> Iterator[TextIO]:
    lock = directory / "active.lock"
    lock.touch(exist_ok=False)
    try:
        with (directory / "receipts.txt").open("a+", encoding="utf-8") as journal:
            yield journal
    finally:
        if not keep_lock:
            lock.unlink()
