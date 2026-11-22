import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

import corpus


FIXTURES = Path(__file__).resolve().parent / "fixtures" / "python_aqa"


class CorpusChecks(unittest.TestCase):
    case = {"oracle_file": "oracle.py", "reference": "fixture", "test": "oracle.DispatchContractTests.test_one"}

    def check(self, source, **kwargs):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checked = root / "checked"
            checked.mkdir()
            oracle = textwrap.dedent(source).encode("utf-8")
            with patch.object(corpus, "git", return_value=oracle):
                return corpus.grade(root, self.case, checked, **kwargs)

    def test_unittest_reports_behavior_failure_and_real_pass(self):
        methods = (("self.fail('BAD_RESULT')", "fail"),
                   ("raise TimeoutError('deadline violated')", "fail"),
                   ("self.assertEqual(2 + 2, 4)", "pass"))
        for method, expected in methods:
            with self.subTest(expected=expected):
                result = self.check(f"""
                    import unittest
                    class DispatchContractTests(unittest.TestCase):
                        def test_one(self):
                            {method}
                """)
                self.assertEqual(result["status"], expected, result)

    def test_unittest_cannot_pass_without_an_executed_check(self):
        for body in (
            "def test_other(self): pass",
            "def test_one(self): self.skipTest('missing tool')",
            "@unittest.expectedFailure\ndef test_one(self): self.fail('known failure')",
            "@unittest.expectedFailure\ndef test_one(self): pass",
            "def test_one(self): __import__('os')._exit(0)",
        ):
            with self.subTest(body=body):
                source = "import unittest\nclass DispatchContractTests(unittest.TestCase):\n" + textwrap.indent(body, "    ")
                result = self.check(source)
                self.assertEqual(result["status"], "unavailable", result)

    def test_unittest_setup_and_dependency_errors_are_unavailable(self):
        sections = {
            "broken-oracle": "def this_oracle_cannot_compile!",
            "missing-dependency": "import fstack_nonexistent_dependency",
            "integration-tool-missing": """
                import subprocess
                import unittest
                class DispatchContractTests(unittest.TestCase):
                    def test_one(self):
                        subprocess.run(['fstack-nonexistent-tool'], check=True)
            """,
            "test-setup": """
                import unittest
                class DispatchContractTests(unittest.TestCase):
                    def setUp(self):
                        self.fail('fixture could not start')
                    def test_one(self):
                        pass
            """,
            "module-setup": """
                import unittest
                def setUpModule():
                    raise RuntimeError('fixture could not start')
                class DispatchContractTests(unittest.TestCase):
                    def test_one(self):
                        pass
            """,
            "build-error": """
                import subprocess
                import unittest
                class DispatchContractTests(unittest.TestCase):
                    def test_one(self):
                        raise subprocess.CalledProcessError(1, 'build')
            """,
        }
        for name, source in sections.items():
            with self.subTest(name=name):
                result = self.check(source)
                self.assertEqual(result["status"], "unavailable", result)

    def test_grade_reports_launch_errors_and_timeouts(self):
        self.assertEqual(self.check("", command_prefix=["fstack-nonexistent-command"])["status"], "unavailable")
        with patch.object(corpus.subprocess, "run", side_effect=subprocess.TimeoutExpired(["python"], 1)):
            self.assertEqual(self.check("")["status"], "unavailable")

    def test_bank_limits_the_module_and_test_class(self):
        for field, value in (("oracle_file", "../oracle.py"), ("oracle_file", "alternate_oracle.py"),
                             ("test", "oracle.OwnTests.test_one"),
                             ("test", "oracle.DispatchContractTests.test_one; command")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus.oracle_test({**self.case, field: value})
        self.assertEqual(corpus.oracle_test(self.case), ("DispatchContractTests", "test_one", ("checks", "dispatch")))

    def test_validate_flags_empty_bank(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bank = root / "bank.json"
            bank.write_text('{"cases": []}', encoding="utf-8")
            self.assertFalse(corpus.validate(root, bank, root / "output"))

    def test_pinned_synthetic_snapshots_and_expected_failures(self):
        bank = FIXTURES / "bank.json"
        with tempfile.TemporaryDirectory() as temporary, contextlib.redirect_stdout(io.StringIO()):
            root = Path(temporary)
            repo = root / "repo"
            repo.mkdir()
            corpus.git(repo, "init", "--object-format=sha1", "-q")
            (repo / ".gitattributes").write_text("* text=auto eol=lf\n", encoding="utf-8")
            for version in ("base", "reference"):
                shutil.copytree(FIXTURES / version, repo / version, ignore=shutil.ignore_patterns("__pycache__"))
            corpus.git(repo, "add", "--", ".gitattributes", "base", "reference")
            root_tree = corpus.git(repo, "write-tree").decode().strip()
            cases = corpus.cases(bank)
            for case in cases:
                for version in ("base", "reference"):
                    with self.subTest(case=case["id"], version=version):
                        self.assertEqual(case[version], corpus.git(repo, "rev-parse", f"{root_tree}:{version}").decode().strip())

            case = cases[0]
            solver = root / "solver"
            corpus.materialize(repo, case, solver)
            self.assertEqual(set(corpus.snapshot(solver)), {"dispatch.py", "checks.py"})
            reference = root / "reference"
            corpus.materialize(repo, case, reference, reference=True)
            self.assertEqual(set(corpus.snapshot(reference)), {"dispatch.py", "checks.py"})
            artifact = corpus.snapshot(solver)
            checked = root / "checked"
            shutil.copytree(solver, checked)
            (checked / "oracle.py").write_text("raise RuntimeError('подменённая проверка')\n", encoding="utf-8")
            self.assertEqual(corpus.grade(repo, case, checked)["status"], "fail")
            self.assertEqual(corpus.snapshot(solver), artifact)

            output = root / "validation"
            valid = corpus.validate(repo, bank, output)
            results = json.loads((output / "validation.json").read_text(encoding="utf-8"))
            self.assertTrue(valid, [r for r in results if not r["valid"]])
            self.assertEqual({(r["split"], r["base"]["status"], r["reference"]["status"]) for r in results},
                             {("control", "pass", "pass"), ("development", "fail", "pass")})


class ReceiptExampleChecks(unittest.TestCase):
    def test_documented_local_and_quick_selections(self):
        example = corpus.ROOT / "docs" / "examples" / "receipt"
        env = {key: value for key, value in os.environ.items() if key.upper() not in {
            "PYTEST_ADDOPTS", "PYTEST_PLUGINS", "PYTHONPATH", "PYTHONHOME"}}
        env["PYTEST_DISABLE_PLUGIN_AUTOLOAD"] = "1"
        scenarios = ((["tests/unit"], 2, 0), (["-m", "quick"], 3, 0),
                     (["tests/unit", "--keep-lock"], 2, 0), (["-m", "quick", "--keep-lock"], 3, 1))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / "receipt"
            shutil.copytree(example, workspace, ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache"))
            for number, (args, total, failures) in enumerate(scenarios):
                with self.subTest(args=args):
                    report = root / f"receipt-{number}.xml"
                    proc = subprocess.run([sys.executable, "-B", "-X", "utf8", "-m", "pytest", "-q",
                        "-p", "no:cacheprovider", f"--junitxml={report}", *args], cwd=workspace, env=env,
                        capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
                    output = proc.stdout + proc.stderr
                    self.assertEqual(proc.returncode, failures, output)
                    suite = ET.parse(report).getroot().find("testsuite")
                    self.assertIsNotNone(suite, output)
                    self.assertEqual({name: int(suite.attrib[name]) for name in ("tests", "failures", "errors", "skipped")},
                                     {"tests": total, "failures": failures, "errors": 0, "skipped": 0}, output)
                    if failures:
                        failed = suite.find("testcase[failure]")
                        self.assertEqual(failed.attrib["name"], "test_second_shift_reads_first_receipt")
                        self.assertIn("FileExistsError", failed.find("failure").attrib["message"])
