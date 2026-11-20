from functools import partial
from pathlib import Path

import pytest

from receipt_journal import open_journal


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption("--keep-lock", action="store_true", help="Оставить файл блокировки после закрытия журнала")


@pytest.fixture
def open_shift(tmp_path: Path, pytestconfig: pytest.Config):
    return partial(open_journal, tmp_path, keep_lock=pytestconfig.getoption("--keep-lock"))
