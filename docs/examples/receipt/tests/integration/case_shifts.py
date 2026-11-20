import pytest


@pytest.mark.quick
def test_second_shift_reads_first_receipt(open_shift):
    with open_shift() as first_shift:
        first_shift.write("R-705\n")

    with open_shift() as second_shift:
        second_shift.seek(0)
        assert second_shift.read().splitlines() == ["R-705"]
