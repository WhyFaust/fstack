import io
import json
import contextlib
from pathlib import Path
import py_compile
import queue
import tarfile
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import app_server
import corpus
import evaluate
import verdicts


class EvaluationTests(unittest.TestCase):
    def test_repetitions_have_one_attempt_per_variant_in_randomized_order(self):
        plan = {"cases": ["one", "two"], "repetitions": 3, "seed": 17,
                "variants": [{"id": "a"}, {"id": "b"}]}
        order = evaluate.schedule(plan)
        self.assertEqual(len(order), 12)
        self.assertEqual(len({evaluate.attempt_id(x) for x in order}), 12)
        self.assertEqual(order, evaluate.schedule(plan))
        self.assertNotEqual(order, evaluate.schedule({**plan, "seed": 19}))
        with self.assertRaises(ValueError):
            evaluate.schedule({**plan, "cases": ["one", "one", "two"]})

    def test_reservation_keeps_first_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "result.json"
            self.assertTrue(evaluate.reserve(target, {"status": "interrupted"}))
            self.assertFalse(evaluate.reserve(target, {"status": "completed"}))
            self.assertEqual(evaluate.read(target), {"status": "interrupted"})

    def test_judge_preserves_interruption_and_refuses_changed_artifacts(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            root = Path(directory)
            plan = {"cases": ["one"], "repetitions": 1, "seed": 17,
                    "variants": [{"id": "baseline"}, {"id": "candidate"}]}
            args = SimpleNamespace(plan=root / "plan.json", bank=root / "bank.json", repo=root,
                codex="codex", auth_home=root, max_attempts=1, model="model", effort="low")
            evaluate.write(args.plan, plan)
            evaluate.write(root / "calibration.json", {"model": "model", "effort": "low", "client_version": "fixture"})
            for variant in ("baseline", "candidate"):
                target = root / "attempts" / f"one-1-{variant}"
                (target / "artifact").mkdir(parents=True)
                (target / "artifact/source.py").write_text("answer = 42\n", encoding="utf-8")
                (target / "change.patch").write_text("", encoding="utf-8")
                evaluate.write(target / "result.json", {"status": "completed", "case": "one", "repeat": 0,
                    "variant": variant, "final": "Done", "plan_sha256": corpus.digest(plan),
                    "patch_sha256": corpus.digest(""), "artifact_sha256": corpus.digest(corpus.snapshot(target / "artifact"))})
            for target, name, options in [
                (evaluate, "check_plan", {}), (evaluate, "report", {}),
                (evaluate, "calibration_ready", {"return_value": True}),
                (evaluate.subprocess, "check_output", {"return_value": "fixture"}),
                (corpus, "cases", {"return_value": [{"id": "one", "prompt": "Task", "criteria": ["Correct"]}]}),
                (corpus, "materialize", {}),
            ]:
                stack.enter_context(patch.object(target, name, **options))
            assessor = stack.enter_context(patch.object(evaluate, "assess", side_effect=KeyboardInterrupt))
            with self.assertRaises(KeyboardInterrupt):
                evaluate.judge(args)
            judgment = root / "judgments/one-1-baseline-vs-candidate.json"
            self.assertEqual(evaluate.read(judgment)["status"], "interrupted")
            evaluate.judge(args)
            self.assertEqual(assessor.call_count, 1)
            judgment.unlink()
            (root / "attempts/one-1-candidate/artifact/source.py").write_text("answer = 0\n", encoding="utf-8")
            with self.assertRaises(ValueError):
                evaluate.judge(args)
            self.assertEqual(assessor.call_count, 1)

    def test_expired_deadline_does_not_drain_an_active_stream(self):
        server = app_server.Server.__new__(app_server.Server)
        server.messages = queue.Queue()
        server.messages.put({"method": "item/started"})
        with self.assertRaises(TimeoutError):
            server.receive(time.monotonic() - 1)

    def test_usage_updates_replace_totals_and_reasoning_items_are_not_saved(self):
        result = {"usage": {}, "items": [], "errors": [], "reroutes": []}
        for total in (10, 25):
            app_server.observe(result, {"method": "thread/tokenUsage/updated", "params": {
                "threadId": "root", "tokenUsage": {"total": {"totalTokens": total}}}})
        app_server.observe(result, {"method": "item/completed", "params": {
            "item": {"type": "reasoning", "text": "must not be stored"}}})
        self.assertEqual(result["usage"]["root"]["total"]["totalTokens"], 25)
        self.assertEqual(result["items"], [])

    def test_archive_cannot_escape_with_windows_paths(self):
        for name in ("../escape", "C:/escape", "C:escape", "safe/file:stream", "safe/CON", "safe./file"):
            archive = io.BytesIO()
            with tarfile.open(fileobj=archive, mode="w") as tar:
                member = tarfile.TarInfo(name)
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))
            with tempfile.TemporaryDirectory() as directory, patch.object(corpus, "git", return_value=archive.getvalue()):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    corpus.extract(Path(directory), "reference", Path(directory) / "output", ["."])

    def test_malformed_or_incomplete_judgment_is_not_a_verdict(self):
        for criteria in (["looks good"], [], [{"criterion": "x", "A": "pass", "B": "pass"}]):
            self.assertFalse(evaluate.valid_judgment({"winner": "A", "reason": "Reason", "criteria": criteria}, ["x"]))

    def test_candidate_syntax_error_is_failure_not_environment_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "scripts/tests/test_memory_store.py"
            path.parent.mkdir(parents=True)
            case = {"oracle_file": "scripts/tests/test_memory_store.py", "reference": "ref",
                    "test": "scripts.tests.test_memory_store.MemoryStoreTests.test_one"}
            with patch.object(corpus, "git", return_value=b"def invalid syntax!\n"):
                self.assertEqual(corpus.grade(root, case, root)["status"], "fail")

    def test_grade_uses_saved_source_instead_of_solver_bytecode(self):
        case = {"id": "cache", "prompt": "Исправь реализацию", "oracle_file": "scripts/tests/test_memory_store.py",
                "reference": "ref", "test": "scripts.tests.test_memory_store.MemoryStoreTests.test_one"}
        oracle = b'''import unittest, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import implementation
class MemoryStoreTests(unittest.TestCase):
    def test_one(self): self.assertEqual(implementation.answer, 42)
'''
        def materialize(repo, case, target, reference=False):
            target.mkdir(parents=True)
            (target / "implementation.py").write_text("answer = 42\n", encoding="utf-8")

        def solve(executable, home, workspace, *args):
            source = workspace / "implementation.py"
            py_compile.compile(str(source), invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
            source.write_text("answer = 0\n", encoding="utf-8")
            return {"status": "completed"}

        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            root = Path(directory)
            args = SimpleNamespace(plan=root / "plan.json", bank=root / "bank.json", repo=root,
                                   codex="codex", auth_home=root, max_attempts=1)
            evaluate.write(args.plan, {"variants": [{"id": "candidate"}], "model": "model",
                                      "effort": "low", "timeout_seconds": 30})
            for target, name, options in [
                (evaluate, "check_plan", {}), (evaluate, "report", {}),
                (evaluate, "schedule", {"return_value": [{"case": "cache", "repeat": 0, "variant": "candidate"}]}),
                (evaluate, "isolation", {"return_value": {"passed": True}}),
                (evaluate, "sandbox_prefix", {"return_value": []}),
                (corpus, "cases", {"return_value": [case]}),
                (corpus, "materialize", {"side_effect": materialize}),
                (corpus, "git", {"return_value": oracle}),
                (evaluate.subprocess, "check_output", {"return_value": "fixture-client"}),
                (app_server, "authentication", {"return_value": contextlib.nullcontext()}),
                (app_server, "run", {"side_effect": solve}),
            ]:
                stack.enter_context(patch.object(target, name, **options))
            evaluate.execute(args)
            result = evaluate.read(root / "attempts/cache-1-candidate/result.json")
            self.assertEqual(result["check"]["status"], "fail")
            self.assertFalse(list((root / "attempts/cache-1-candidate/artifact").rglob("*.pyc")))

    def fixture(self, repetitions=3):
        plan = {"cases": ["development", "holdout", "control"], "repetitions": repetitions}
        cases = {c: {"split": c} for c in plan["cases"]}
        runs, judgments = [], []
        for case in cases:
            for repeat in range(repetitions):
                for variant in ("baseline", "candidate"):
                    runs.append({"case": case, "repeat": repeat, "variant": variant,
                                 "status": "completed", "check": {"status": "pass"}})
                judgments.append({"case": case, "repeat": repeat, "comparator": "baseline",
                    "candidate": "candidate", "status": "completed", "fresh": True,
                    "labels": {"A": "baseline", "B": "candidate"}, "verdict": {
                        "winner": "tie" if case == "control" else "B",
                        "criteria": [{"A": "pass", "B": "pass"}]}})
        return plan, cases, runs, judgments

    def decide(self, fixture, calibrated=True, stale=False):
        return verdicts.compare(*fixture, "baseline", "candidate", calibrated, stale)["decision"]

    def test_accept_requires_repeat_holdout_control_and_calibrated_judge(self):
        self.assertEqual(self.decide(self.fixture()), "accept")
        self.assertEqual(self.decide(self.fixture(1)), "inconclusive")
        self.assertEqual(self.decide(self.fixture(), calibrated=False), "inconclusive")
        self.assertEqual(self.decide(self.fixture(), stale=True), "inconclusive")

    def test_regression_and_missing_runs_cannot_be_compensated_by_other_wins(self):
        data = self.fixture()
        data[2][-1]["check"]["status"] = "fail"
        self.assertEqual(self.decide(data), "revise")
        data[2][-1].update(status="timeout", check={})
        self.assertEqual(self.decide(data), "inconclusive")

    def test_ties_reject_an_unproven_change(self):
        data = self.fixture()
        for judgment in data[3]:
            judgment["verdict"]["winner"] = "tie"
        self.assertEqual(self.decide(data), "reject")

    def test_shared_critical_failure_blocks_accept_even_when_judge_prefers_candidate(self):
        data = self.fixture()
        for judgment in data[3]:
            judgment["verdict"]["criteria"] = [{"A": "fail", "B": "fail"}]
        self.assertEqual(self.decide(data), "revise")
        self.assertEqual(self.decide(data, calibrated=False), "inconclusive")

    def test_executable_does_not_grant_read_access_to_its_parent(self):
        import tomllib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "codex"
            binary.write_bytes(b"fixture")
            workspace = root / "workspace"
            workspace.mkdir()
            app_server.profile(root / "home", workspace, executable=str(binary))
            permissions = tomllib.loads((root / "home/config.toml").read_text(encoding="utf-8"))["permissions"]["fstack-eval"]["filesystem"]
            self.assertEqual(permissions[str(binary.resolve())], "read")
            self.assertNotIn(str(root.resolve()), permissions)
            self.assertNotIn(str(root.parent.resolve()), permissions)


if __name__ == "__main__":
    unittest.main()
