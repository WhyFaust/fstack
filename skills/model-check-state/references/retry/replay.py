import argparse
from pathlib import Path


def replay(events: list[str], deduplicate: bool) -> None:
    request_id = "order-17"
    ledger: list[str] = []
    phase = "ready"
    attempts = 0

    for event in events:
        if event == "send" and phase == "ready" and attempts < 2:
            attempts += 1
            phase = "in_flight"
        elif event == "commit" and phase == "in_flight":
            if not deduplicate or request_id not in ledger:
                ledger.append(request_id)
            phase = "reply_pending"
        elif event == "lose_reply" and phase == "reply_pending" and attempts == 1:
            phase = "ready"
        else:
            raise ValueError(f"Unexpected event {event!r} in {phase}")
        print(f"{event} id={request_id} commits={ledger.count(request_id)}")

    commits = ledger.count(request_id)
    expected = 1 if deduplicate else 2
    if phase != "reply_pending" or attempts != 2 or commits != expected:
        raise ValueError(f"Expected two attempts and {expected} commits, got {attempts=} {commits=}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("broken", "fixed"))
    parser.add_argument("trace", type=Path)
    args = parser.parse_args()
    replay(args.trace.read_text(encoding="utf-8").splitlines(), args.mode == "fixed")


if __name__ == "__main__":
    main()
