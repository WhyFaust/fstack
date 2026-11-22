from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from benchmark import HookRunner, catalog_signals, expect_context, fingerprint, measure, read_result, summary


def active_response(root: Path) -> dict:
    context = "\n".join([
        "В этом чате включён faust-mode до команды /faust-mode-off.",
        str(root / "skills/faust-mode/SKILL.md"),
        str(root / "skills/route/SKILL.md"),
        str(root / "docs/skills.md"),
        "faust-mode — Координировать работу",
        "route — Выбрать метод",
    ])
    return {"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context}}


def recorded_result() -> dict:
    samples = [1, 2, 3, 4, 20]
    return {
        "schema": 2, "kind": "coordinator", "binary_sha256": "a" * 64,
        "inputs": {name: "b" * 64 for name in ("runtime_sha256", "package_sha256", "harness_sha256")},
        "settings": {"samples": 5, "methods": 2, "event": "UserPromptSubmit"},
        "results": {
            name: {
                "samples_ms": list(samples), "median_ms": 3, "p95_ms": 20,
                "verified": True, "warmups": int(name == "warm_continuation"),
                "responses": [{"response_sha256": "c" * 64, "context_bytes": 300,
                               "index_entries": 2 if name == "explicit_activation" else 0}
                              for _ in samples],
            } for name in ("cold_inactive", "explicit_activation", "warm_continuation")
        },
    }


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

    def test_catalog_covers_installed_methods_and_omits_translation_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("faust-mode", "route"):
                path = root / "skills" / name / "SKILL.md"
                path.parent.mkdir(parents=True)
                path.write_text("method", encoding="utf-8")
            catalog = root / "docs/skills.md"
            catalog.parent.mkdir()
            rows = [
                "| [faust-mode](../skills/faust-mode/SKILL.md) | Координировать работу |",
                "| [route](../skills/route/SKILL.md) | Выбрать метод, [EN](../skills/route/SKILL.en.md) |",
            ]
            catalog.write_text("\n".join(rows), encoding="utf-8")
            self.assertEqual({"faust-mode": "Координировать работу", "route": "Выбрать метод"}, catalog_signals(root))
            for invalid in ([rows[0]], [*rows, rows[0]], [rows[0], rows[1].replace("Выбрать метод, ", "")]):
                with self.subTest(rows=invalid), self.assertRaises(ValueError):
                    catalog.write_text("\n".join(invalid), encoding="utf-8")
                    catalog_signals(root)

    def test_context_verifies_activation_and_omits_repeated_index(self):
        root = Path("package").resolve()
        signals = {"faust-mode": "Координировать работу", "route": "Выбрать метод"}
        response = active_response(root)
        observation = expect_context(response, signals, root, active=True, index=True)
        self.assertEqual(2, observation["index_entries"])
        self.assertEqual(len(response["hookSpecificOutput"]["additionalContext"].encode()), observation["context_bytes"])
        self.assertEqual(64, len(observation["response_sha256"]))
        continuation = deepcopy(response)
        continuation["hookSpecificOutput"]["additionalContext"] = "\n".join(
            response["hookSpecificOutput"]["additionalContext"].splitlines()[:-2])
        self.assertEqual(0, expect_context(continuation, signals, root, active=True, index=False)["index_entries"])
        inactive = {"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "Faust-mode выключен."}}
        self.assertEqual(0, expect_context(inactive, signals, root, active=False, index=False)["index_entries"])

    def test_fast_wrong_or_partial_context_is_rejected(self):
        root = Path("package").resolve()
        signals = {"faust-mode": "Координировать работу", "route": "Выбрать метод"}
        response = active_response(root)
        context = response["hookSpecificOutput"]["additionalContext"]
        invalid_contexts = [
            "", context.replace("до команды /faust-mode-off", "на этот запрос"),
            context.replace(str(root / "docs/skills.md"), "unknown"),
            context.replace("route — Выбрать метод", ""),
            context + "\nroute — Выбрать метод",
            context.replace("route — Выбрать метод", "route — Другое описание"),
            context.replace("faust-mode — Координировать работу\nroute — Выбрать метод",
                            "route — Выбрать метод\nfaust-mode — Координировать работу"),
        ]
        invalid_responses = [
            {}, [], {**response, "systemMessage": "message"},
            {"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}},
            *[{"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": value}}
              for value in invalid_contexts],
        ]
        for value in invalid_responses:
            with self.subTest(response=value), self.assertRaises(ValueError):
                expect_context(value, signals, root, active=True, index=True)
        for active in (False, True):
            with self.subTest(unexpected_index=active), self.assertRaises(ValueError):
                expect_context(response, signals, root, active=active, index=False)

    def test_hook_process_uses_isolated_profile_and_official_entrypoint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            completed = SimpleNamespace(returncode=0, stderr=b"", stdout=json.dumps({
                "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "Faust-mode выключен."}
            }).encode())
            with patch.dict(os.environ, {"CODEX_HOME": "/live-profile", "FSTACK_FAULT": "1", "API_TOKEN": "secret"}), \
                    patch("benchmark.subprocess.run", return_value=completed) as run:
                runner = HookRunner(root, root / "fstack", root / "package", {"route": "Выбрать метод"})
                runner.invoke("Проверь модуль.", "turn-1", active=False, index=False)
            command = run.call_args.args[0]
            options = run.call_args.kwargs
            self.assertEqual([str(root / "fstack"), "--package", str(root / "package"), "hook", "UserPromptSubmit"], command)
            self.assertEqual(str(root / "codex"), options["env"]["CODEX_HOME"])
            self.assertNotIn("FSTACK_FAULT", options["env"])
            self.assertNotIn("API_TOKEN", options["env"])
            self.assertEqual(str(root / "project"), json.loads(options["input"])["cwd"])
            self.assertEqual("turn-1", json.loads(options["input"])["turn_id"])
            self.assertEqual(10, options["timeout"])

    def test_measure_excludes_preparation_and_one_warmup(self):
        clock = [0]
        observed = []

        def prepare(index):
            clock[0] += 5_000_000

            def operation():
                clock[0] += 2_000_000
                observed.append(index)
                return {"sample": index}
            return operation

        with patch("benchmark.time.perf_counter_ns", side_effect=lambda: clock[0]):
            result = measure(prepare, 5, warmups=1)
        self.assertEqual([-1, 0, 1, 2, 3, 4], observed)
        self.assertEqual([2.0] * 5, result["samples_ms"])
        self.assertEqual([{"sample": i} for i in range(5)], result["responses"])
        self.assertEqual(2.0, result["median_ms"])

    def test_summary_rejects_invalid_samples_and_uses_upper_rank(self):
        self.assertEqual({"median_ms": 3, "p95_ms": 20}, summary([1, 2, 3, 4, 20]))
        self.assertEqual(19, summary(list(range(1, 21)))["p95_ms"])
        for invalid in ([], [True], [-1], [float("nan")], [float("inf")], ["1"]):
            with self.subTest(samples=invalid), self.assertRaises(ValueError):
                summary(invalid)

    def test_report_rejects_forged_aggregates_missing_samples_and_wrong_evidence(self):
        data = recorded_result()
        mutations = [
            ("schema", 1), ("settings.samples", 5.0), ("settings.methods", 2.5),
            ("settings.event", "SessionStart"), ("binary_sha256", "unknown"),
            ("results.cold_inactive.median_ms", 1),
            ("results.cold_inactive.samples_ms", [1, 2, 3, 4]),
            ("results.cold_inactive.samples_ms", [True, 2, 3, 4, 20]),
            ("results.cold_inactive.verified", False),
            ("results.warm_continuation.warmups", 0),
            ("results.explicit_activation.responses", []),
            ("results.explicit_activation.responses", [
                {"response_sha256": "c" * 64, "context_bytes": 300, "index_entries": 1} for _ in range(5)]),
            ("results.cold_inactive.responses", [
                {"response_sha256": "unknown", "context_bytes": 300, "index_entries": 0} for _ in range(5)]),
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            self.assertEqual(data, read_result(path))
            for field, value in mutations:
                with self.subTest(field=field), self.assertRaises(ValueError):
                    invalid = deepcopy(data)
                    target = invalid
                    *parents, key = field.split(".")
                    for parent in parents:
                        target = target[parent]
                    target[key] = value
                    path.write_text(json.dumps(invalid), encoding="utf-8")
                    read_result(path)
            del data["results"]["warm_continuation"]
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ValueError):
                read_result(path)


if __name__ == "__main__":
    unittest.main()
