import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def executable():
    configured = os.environ.get('FSTACK_CHECK_BIN')
    binary = Path(configured) if configured else ROOT / 'tools/fstack/target/release' / (
        'fstack-check.exe' if os.name == 'nt' else 'fstack-check')
    if not binary.is_file():
        raise ValueError('Нативный валидатор не найден. Соберите его: '
                         'cargo build --release --locked --manifest-path tools/fstack/Cargo.toml '
                         'или задайте FSTACK_CHECK_BIN.')
    return str(binary.resolve())


def native(*arguments, payload=None):
    result = subprocess.run([executable(), *map(str, arguments)], input=payload,
                            text=True, encoding='utf-8', capture_output=True, check=False)
    if result.returncode:
        raise ValueError(result.stderr.strip() or result.stdout.strip())
    return result.stdout


def skill_metadata(path):
    return json.loads(native('metadata', path))


def skill_resources(folder, root):
    native('resources', root, folder)


def role_settings(role, name):
    native('role-settings', name, payload=json.dumps(role, ensure_ascii=False))


def check(root=ROOT):
    print(native('check-package', root), end='')


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    try:
        check()
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
