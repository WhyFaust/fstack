"""Общие проверки репозитория fstack для локального запуска и CI."""

import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys


def run(*command):
    print(f"+ {shlex.join(map(str, command))}", flush=True)
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", help="целевая платформа Rust")
    parser.add_argument("--toolchain", default="1.90.0", help="версия toolchain Rust")
    parser.add_argument("--binary", type=Path, help="готовый бинарник из сборки CI")
    parser.add_argument("--skip-python-tests", action="store_true",
                        help="в матрице CI тесты Python выполняются на Linux")
    args = parser.parse_args()

    manifest = Path("tools/fstack/Cargo.toml")
    if not manifest.is_file():
        parser.error("запустите команду из корня fstack")
    target_dir = manifest.parent / "target"
    cargo = ("cargo", f"+{args.toolchain}")
    project = ("--manifest-path", manifest)
    options = ("--locked", *project, "--target-dir", target_dir)
    if args.target:
        options += ("--target", args.target)
        target_dir /= args.target

    run(*cargo, "fmt", *project, "--", "--check")
    run(*cargo, "clippy", "--all-targets", *options, "--", "-D", "warnings")
    run(*cargo, "test", "--release", *options)
    binary = args.binary
    if binary is None:
        run(*cargo, "build", "--release", *options)
        binary = target_dir / "release" / ("fstack.exe" if os.name == "nt" else "fstack")
    run(binary.resolve(), "--package", ".", "check-package", ".")
    run(binary.resolve(), "--package", ".", "release", "check")

    if args.skip_python_tests:
        print("Пропуск тестов Python. В CI их запускает задача Linux.", flush=True)
    else:
        for suite in ("benchmarks", "evals"):
            run(sys.executable, "-B", "-X", "utf8", "-m", "unittest", "discover", "-s", f"tools/fstack/{suite}", "-v")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    try:
        main()
    except (OSError, subprocess.CalledProcessError) as exc:
        print(exc, file=sys.stderr)
        sys.exit(1)
