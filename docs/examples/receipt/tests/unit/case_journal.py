import pytest


@pytest.mark.quick
def test_new_journal_is_empty(open_shift):
    with open_shift() as journal:
        assert journal.read() == ""


@pytest.mark.quick
def test_receipt_is_visible_in_current_shift(open_shift):
    with open_shift() as journal:
        journal.write("R-140\n")
        journal.seek(0)
        assert journal.read().splitlines() == ["R-140"]
