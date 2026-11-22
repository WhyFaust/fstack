import asyncio
from contextlib import chdir
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import checks
import dispatch


class DispatchContractTests(unittest.TestCase):
    def test_manifest_drains_sibling_after_failure(self):
        async def verify():
            started, cleaned = asyncio.Event(), asyncio.Event()

            async def pending():
                started.set()
                try:
                    await asyncio.Future()
                finally:
                    cleaned.set()

            async def rejected():
                await started.wait()
                raise ValueError("carrier refused")

            reasons = []
            try:
                async with asyncio.timeout(2):
                    await dispatch.issue_manifest([pending, rejected])
            except* ValueError as group:
                reasons.extend(str(error) for error in group.exceptions)
            self.assertEqual(reasons, ["carrier refused"])
            self.assertTrue(cleaned.is_set(), "MANIFEST_DRAINS_SIBLING")

        asyncio.run(verify())

    def test_sends_return_individual_outcomes(self):
        async def verify():
            last_finished = asyncio.Event()
            failure = ValueError("carrier refused")

            async def first():
                await last_finished.wait()
                return "north"

            async def rejected():
                raise failure

            async def last():
                last_finished.set()
                return "west"

            outcomes = None
            try:
                async with asyncio.timeout(2):
                    outcomes = await dispatch.send_individually([first, rejected, last])
            except* ValueError as group:
                self.assertEqual(group.exceptions, (failure,))
            self.assertEqual(outcomes, ["north", failure, "west"], "SENDS_RETURN_INDIVIDUAL_OUTCOMES")

        asyncio.run(verify())

    def test_successful_batches_preserve_input_order(self):
        async def verify(batch):
            last_finished = asyncio.Event()

            async def first():
                await last_finished.wait()
                return "north"

            async def last():
                last_finished.set()
                return "west"

            async with asyncio.timeout(2):
                self.assertEqual(await batch([first, last]), ["north", "west"])
                self.assertEqual(await batch([]), [])

        for batch in (dispatch.issue_manifest, dispatch.send_individually):
            with self.subTest(batch=batch.__name__):
                asyncio.run(verify(batch))

    def run_project_check(self, name):
        result = unittest.TestResult()
        checks.DispatchTests(name).run(result)
        self.assertEqual(result.testsRun, 1)
        self.assertEqual(result.skipped, [])
        self.assertEqual(result.errors, [])
        self.assertEqual(result.expectedFailures, [])
        self.assertEqual(result.unexpectedSuccesses, [])
        return result

    def test_tracking_check_rejects_missing_label(self):
        with patch.object(dispatch, "make_label", return_value=None):
            result = self.run_project_check("test_tracking_code")
        self.assertEqual(len(result.failures), 1, "CHECK_REJECTS_MISSING_LABEL")
        for label in ({}, {"tracking_code": "QZ-000"}):
            with self.subTest(label=label), patch.object(dispatch, "make_label", return_value=label):
                self.assertEqual(len(self.run_project_check("test_tracking_code").failures), 1)

    def test_invoice_check_detects_wrong_total(self):
        calculate = dispatch.calculate_total
        with patch.object(dispatch, "calculate_total", side_effect=lambda lines: calculate(lines) + 1):
            result = self.run_project_check("test_invoice_total")
        self.assertEqual(len(result.failures), 1, "CHECK_DETECTS_WRONG_INVOICE_TOTAL")

    def test_project_checks_accept_valid_data(self):
        for name in ("test_tracking_code", "test_invoice_total"):
            with self.subTest(name=name):
                self.assertEqual(self.run_project_check(name).failures, [])

    def test_failed_setup_restores_state_before_next_test(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        original_directory = Path.cwd()
        self.enterContext(chdir(original_directory))
        self.enterContext(patch.dict(os.environ, {"DISPATCH_REGION": "south"}))
        settings = {"region": "south", "currency": "RUB"}

        def failed_read():
            self.assertEqual((Path.cwd(), os.environ["DISPATCH_REGION"], settings["region"]),
                             (directory, "north", "north"))
            settings["extra"] = "partial"
            raise ValueError("invalid region settings")

        class SequentialTests(unittest.TestCase):
            def test_initialization(self):
                with self.assertRaisesRegex(ValueError, "invalid region settings"):
                    with dispatch.dispatch_environment(directory, settings, read_settings=failed_read):
                        self.fail("Подготовка должна прервать вход в контекст")

            def test_next(self):
                self.assertEqual((Path.cwd(), os.environ.get("DISPATCH_REGION"), settings),
                                 (original_directory, "south", {"region": "south", "currency": "RUB"}),
                                 "SETUP_STATE_RESTORED")

        result = unittest.TestResult()
        unittest.TestSuite([SequentialTests("test_initialization"), SequentialTests("test_next")]).run(result)
        self.assertEqual(result.testsRun, 2)
        self.assertEqual(result.errors, [])
        self.assertEqual(result.skipped, [])
        self.assertEqual(result.failures, [])

    def test_successful_setup_restores_original_absence(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        original_directory = Path.cwd()
        self.enterContext(chdir(original_directory))
        self.enterContext(patch.dict(os.environ))
        os.environ.pop("DISPATCH_REGION", None)
        settings = {"currency": "RUB"}
        with dispatch.dispatch_environment(directory, settings, read_settings=lambda: {"area": 5}):
            self.assertEqual(Path.cwd(), directory)
            self.assertEqual(os.environ["DISPATCH_REGION"], "north")
            self.assertEqual(settings, {"currency": "RUB", "region": "north", "area": 5})
        self.assertEqual(Path.cwd(), original_directory)
        self.assertNotIn("DISPATCH_REGION", os.environ)
        self.assertEqual(settings, {"currency": "RUB"})

    def test_expiry_uses_injected_clock_at_boundary(self):
        clock = Mock(side_effect=[29, 30, 31])
        expired = [dispatch.expired(30, clock=clock) for _ in range(3)]
        self.assertEqual(clock.call_count, 3, "EXPIRY_USES_INJECTED_CLOCK")
        self.assertEqual(expired, [False, True, True])

    def test_expiry_accepts_existing_calls(self):
        self.assertFalse(dispatch.expired(float("inf")))
        self.assertTrue(dispatch.expired(float("-inf")))
