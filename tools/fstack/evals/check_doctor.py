"""Проверяет диагностику fstack на установленном Codex без вызова модели."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from app_server import environment, trust_fixture_hooks


def check(package: Path, codex: str, scratch: Path) -> None:
    home = scratch / "profile"
    project = scratch / "project"
    home.mkdir()
    project.mkdir()
    config = home / "config.toml"
    config.write_text("[features]\napps = false\nremote_plugin = false\nhooks = true\n", encoding="utf-8")
    binary = package / "bin" / ("fstack.exe" if os.name == "nt" else "fstack")
    env = environment(home, isolated_user=True)

    def run(*args: str | Path, code: int = 0, cwd: Path = project) -> str:
        result = subprocess.run(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, encoding="utf-8", errors="replace", timeout=90)
        if result.returncode != code:
            raise RuntimeError(f"{args}: ожидался код {code}, получен {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result.stdout

    def doctor(code: int) -> dict:
        report = json.loads(run(binary, "doctor", "--json", "--codex", codex, code=code))
        assert report["status"] == ("attention" if code else "ok"), report
        assert Path(report["profile"]).samefile(home) and Path(report["project"]).samefile(project), report
        assert not any(project.iterdir()), "doctor создал файл в рабочем проекте"
        return report

    run(codex, "plugin", "marketplace", "add", ".", cwd=package)
    assert doctor(1)["installed"] is False
    installed = json.loads(run(codex, "plugin", "add", "fstack@fstack", "--json"))
    root = Path(installed["installedPath"])
    initial = doctor(1)
    assert initial["installed"] is True and initial["enabled"] is True, initial
    assert initial["version"] == installed["version"] and Path(initial["package"]).samefile(root), initial
    assert initial["runtime_present"] is True and Path(initial["runtime"]).samefile(root / "bin" / binary.name), initial
    assert initial["enabled_skills"] == len(list((package / "skills").glob("*/SKILL.md"))), initial

    expected = trust_fixture_hooks(codex, home, project)
    assert expected and initial["hooks"] == expected, initial
    healthy = doctor(0)
    assert healthy["issues"] == [] and healthy["hooks"] == {
        event: ["trusted"] * len(states) for event, states in expected.items()
    }, healthy
    print(f"{installed['version']}: doctor обнаружил установку, runtime и смену доверия хуков.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--codex", required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fstack-doctor-") as directory:
        check(args.package.resolve(), args.codex, Path(directory))
