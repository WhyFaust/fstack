import json
import hashlib
from pathlib import Path
import tempfile
import unittest

from benchmark import expect_search, fingerprint, read_result, summary


class BenchmarkTests(unittest.TestCase):
    def test_paths_have_platform_independent_order(self):
        names = ["skills/a/SKILL.md", "skills/a/agents/openai.yaml", "skills/a/assets/icon.png"]
        expected = hashlib.sha256(b"".join(name.encode() + b"\0sample\0" for name in names)).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = [root / name for name in reversed(names)]
            for path in paths:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"sample")
            self.assertEqual(expected, fingerprint(paths, root))

    def test_search_rejects_fast_wrong_answer(self):
        expect_search([{"id": "wanted"}], "wanted")
        expect_search([], None)
        for value, expected in [([], "wanted"), ([{"id": "other"}], "wanted"), ([{"id": "wanted"}], None)]:
            with self.assertRaises(ValueError):
                expect_search(value, expected)

    def test_fingerprint_detects_byte_changes_and_moves(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "input.txt"
            path.write_bytes(b"first\r\nsecond\r\n")
            original = fingerprint([path], root)
            path.write_bytes(b"first\nsecond\n")
            self.assertNotEqual(original, fingerprint([path], root))
            path.write_bytes(b"changed\n")
            self.assertNotEqual(original, fingerprint([path], root))
            path.write_bytes(b"first\r\nsecond\r\n")
            moved = path.rename(root / "moved.txt")
            self.assertNotEqual(original, fingerprint([moved], root))

    def test_report_rejects_invented_aggregate_and_missing_sample(self):
        samples = [1, 2, 3, 4, 20]
        result = {"samples_ms": samples, "verified": True, **summary(samples)}
        data = {"schema": 1, "kind": "runtime", "settings": {"samples": 5, "records": 2},
                "results": {name: dict(result) for name in ("show", "search_hit", "search_miss")}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            self.assertEqual(20, read_result(path)["results"]["show"]["p95_ms"])
            for key, invalid in [("records", 2.5), ("samples", 5.0)]:
                with self.subTest(setting=key), self.assertRaises(ValueError):
                    settings = {**data["settings"], key: invalid}
                    path.write_text(json.dumps({**data, "settings": settings}), encoding="utf-8")
                    read_result(path)
            data["results"]["show"]["samples_ms"] = [True, *samples[1:]]
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ValueError):
                read_result(path)
            data["results"]["show"]["samples_ms"] = samples
            data["results"]["show"]["median_ms"] = 1
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ValueError):
                read_result(path)
            data["results"]["show"]["median_ms"] = 3
            data["results"]["show"]["samples_ms"] = samples[:-1]
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ValueError):
                read_result(path)


if __name__ == "__main__":
    unittest.main()
