"""Дочерняя проверка передаётся Python из доверенного runner."""
import subprocess
import sys
import traceback
import types
import unittest
from pathlib import Path


EXIT_CODES = {"pass": 0, "fail": 1, "unavailable": 2}
RESULT_PREFIX = "FSTACK_UNITTEST_RESULT="


def in_product(filename, oracle):
    source = Path(filename).resolve()
    return source != oracle and source.is_relative_to(Path.cwd())


def unavailable(error, detail, candidate_modules):
    if isinstance(error, ImportError):
        return (error.name or "").split(".")[0] not in candidate_modules
    if isinstance(error, (PermissionError, subprocess.SubprocessError)):
        return True
    return isinstance(error, OSError) and any(
        frame.f_globals.get("__name__") == "subprocess" for frame, _ in traceback.walk_tb(detail)
    )


def run(oracle, class_name, test_name, candidate_modules):
    class Result(unittest.TextTestResult):
        fixture_error = False

        def record_error(self, err):
            _, error, detail = err
            self.fixture_error |= unavailable(error, detail, candidate_modules) or any(
                frame.f_globals.get("__name__") in {"unittest.case", "unittest.async_case"}
                and frame.f_code.co_name in {"_callSetUp", "_callTearDown", "_callCleanup"}
                for frame, _ in traceback.walk_tb(detail)
            )

        def addError(self, test, err):
            self.record_error(err)
            super().addError(test, err)

        def addFailure(self, test, err):
            self.record_error(err)
            super().addFailure(test, err)

        def addSubTest(self, test, subtest, err):
            if err is not None:
                self.record_error(err)
            super().addSubTest(test, subtest, err)

    module = types.ModuleType("_fstack_eval_oracle")
    module.__file__ = str(oracle)
    sys.modules[module.__name__] = module
    try:
        exec(compile(oracle.read_bytes(), oracle, "exec"), vars(module))
    except Exception as error:
        traceback.print_exc()
        if unavailable(error, error.__traceback__, candidate_modules):
            return "unavailable"
        if isinstance(error, ImportError) or (
                isinstance(error, SyntaxError) and error.filename and in_product(error.filename, oracle)
        ) or any(in_product(frame.f_code.co_filename, oracle) for frame, _ in traceback.walk_tb(error.__traceback__)
                 if not frame.f_code.co_filename.startswith("<")):
            return "fail"
        return "unavailable"

    test_class = getattr(module, class_name, None)
    if not isinstance(test_class, type) or not issubclass(test_class, unittest.TestCase):
        print(f"Проверяющий класс недоступен: {class_name}", file=sys.stderr)
        return "unavailable"
    if not callable(getattr(test_class, test_name, None)):
        print(f"Проверка недоступна: {test_name}", file=sys.stderr)
        return "unavailable"
    suite = unittest.TestSuite([test_class(test_name)])
    result = unittest.TextTestRunner(verbosity=2, resultclass=Result).run(suite)
    if (result.testsRun != 1 or result.skipped or result.expectedFailures or result.unexpectedSuccesses
            or result.fixture_error):
        return "unavailable"
    return "pass" if result.wasSuccessful() else "fail"


if __name__ == "__main__":
    oracle, class_name, test_name, *candidate_modules = sys.argv[1:]
    status = run(Path(oracle).resolve(), class_name, test_name, candidate_modules)
    print(RESULT_PREFIX + status)
    raise SystemExit(EXIT_CODES[status])
