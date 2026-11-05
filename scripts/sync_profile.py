import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
ROLES = ('auditor', 'coder', 'reviewer', 'verifier', 'allure_analyst')
LEGACY_ROLES = ('auditor', 'coder', 'python_coder', 'reviewer', 'allure_analyst')
RETIRED_PATHS = tuple(f'agents/{name}.toml' for name in LEGACY_ROLES if name not in ROLES)
START = '<!-- fstack:profile:start -->'
END = '<!-- fstack:profile:end -->'
STATE = 'fstack/profile.json'


class ProfileConflict(ValueError):
    def __init__(self, paths):
        self.paths = sorted(set(paths))
        super().__init__('Конфликт профиля: ' + ', '.join(self.paths))


def digest(content):
    return hashlib.sha256(content).hexdigest()


def target(home, relative):
    path = home / relative
    current = home
    for part in Path(relative).parts:
        current = current / part
        if current.is_symlink() or getattr(current, 'is_junction', lambda: False)():
            raise ProfileConflict([relative])
    if not path.resolve().is_relative_to(home):
        raise ProfileConflict([relative])
    return path


def read_optional(path):
    if not path.exists():
        return None
    if not path.is_file():
        raise ValueError(f'Ожидался файл: {path}')
    return path.read_bytes()


def sources(root):
    manifest = json.loads((root / '.codex-plugin/plugin.json').read_text(encoding='utf-8'))
    version = manifest['version']
    if not isinstance(version, str) or not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Некорректная версия установленного плагина')
    payloads = {}
    for name in ROLES:
        content = (root / f'profile/agents/{name}.toml').read_bytes()
        data = tomllib.loads(content.decode('utf-8'))
        if data.get('name') != name or not data.get('description') or not data.get('developer_instructions'):
            raise ValueError(f'Некорректная роль в пакете: {name}')
        payloads[f'agents/{name}.toml'] = content
    links = (root / 'profile/AGENTS.md').read_text(encoding='utf-8').strip()
    if not links or START in links or END in links:
        raise ValueError('Некорректный управляемый блок в пакете')
    return version, payloads, links


def merge_links(content, links, expected, adopt):
    text = content.decode('utf-8') if content is not None else ''
    newline = '\r\n' if '\r\n' in text else '\n'
    block = START + newline + links.replace('\r\n', '\n').replace('\n', newline) + newline + END
    starts, ends = text.count(START), text.count(END)
    if not starts and not ends:
        if expected is not None:
            raise ProfileConflict(['AGENTS.md:managed-block'])
        separator = newline if text.endswith('\n') else newline * 2
        updated = (text + separator if text else '') + block + newline
    elif starts == ends == 1 and text.index(START) < text.index(END):
        begin = text.index(START)
        end = text.index(END) + len(END)
        current = text[begin:end]
        if expected is not None and digest(current.encode('utf-8')) != expected:
            raise ProfileConflict(['AGENTS.md:managed-block'])
        if expected is None and current != block and not adopt:
            raise ProfileConflict(['AGENTS.md:unmanaged-block'])
        updated = text[:begin] + block + text[end:]
    else:
        raise ProfileConflict(['AGENTS.md:invalid-markers'])
    return updated.encode('utf-8'), digest(block.encode('utf-8'))


def plan(home, root=ROOT, adopt=False):
    version, payloads, links = sources(root)
    state_content = read_optional(target(home, STATE))
    state = json.loads(state_content.decode('utf-8')) if state_content is not None else None
    if state_content is not None:
        if (not isinstance(state, dict) or type(state.get('schema')) is not int
                or state['schema'] != 1 or not isinstance(state.get('files'), dict)):
            raise ValueError('Некорректное состояние fstack/profile.json')
        legacy_paths = {f'agents/{name}.toml' for name in LEGACY_ROLES}
        if set(state['files']) not in (set(payloads), legacy_paths):
            raise ValueError('Состав управляемого профиля отличается от поддерживаемого')
        hashes = [*state['files'].values(), state.get('links_sha256')]
        if any(not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{64}', value) for value in hashes):
            raise ValueError('Некорректные хеши управляемого профиля')
    owned = state['files'] if state is not None else {}
    payloads.update({name: None for name in RETIRED_PATHS if name in owned})
    before = {name: read_optional(target(home, name)) for name in [*payloads, 'AGENTS.md']}
    before[STATE] = state_content
    conflicts = []
    for name, content in payloads.items():
        current = before[name]
        if name in owned:
            if current is None or digest(current) != owned[name]:
                conflicts.append(name)
        elif current is not None and current != content and not adopt:
            conflicts.append(name)
    try:
        payloads['AGENTS.md'], block_hash = merge_links(
            before['AGENTS.md'], links, state['links_sha256'] if state is not None else None, adopt,
        )
    except ProfileConflict as error:
        conflicts.extend(error.paths)
        block_hash = None
    if conflicts:
        raise ProfileConflict(conflicts)
    updated_state = {
        'schema': 1,
        'version': version,
        'files': {f'agents/{name}.toml': digest(payloads[f'agents/{name}.toml']) for name in ROLES},
        'links_sha256': block_hash,
    }
    payloads[STATE] = (json.dumps(updated_state, ensure_ascii=False, indent=2, sort_keys=True) + '\n').encode('utf-8')
    changes = {name: content for name, content in payloads.items() if before[name] != content}
    return version, before, changes


def profile_result(home, status, version, before, changes):
    return {
        'status': status,
        'version': version,
        'changes': list(changes),
        'removed': [name for name, content in changes.items() if content is None],
        'unmanaged_retired': [name for name in RETIRED_PATHS if name not in before and os.path.lexists(home / name)],
    }


def atomic_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix='.fstack-', dir=path.parent)
    temporary = Path(temporary)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def private_backup(path):
    path.mkdir(mode=0o700, parents=True)
    if os.name == 'nt':
        command = """$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$acl = [System.Security.AccessControl.DirectorySecurity]::new()
$acl.SetSecurityDescriptorSddlForm('D:P(A;OICI;FA;;;' + $sid + ')(A;OICI;FA;;;SY)', [System.Security.AccessControl.AccessControlSections]::Access)
[System.IO.Directory]::SetAccessControl($env:FSTACK_BACKUP_PATH, $acl)
"""
        subprocess.run(
            ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', command],
            env={**os.environ, 'FSTACK_BACKUP_PATH': str(path)}, capture_output=True, check=True,
        )
    return path


@contextmanager
def exclusive_profile(home):
    path = target(home, 'fstack/profile.lock')
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as error:
        raise ProfileConflict(['fstack/profile.lock']) from error
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as stream:
            stream.write(str(os.getpid()))
        yield
    finally:
        path.unlink(missing_ok=True)


def apply_profile(home, root=ROOT, adopt=False):
    version, before, changes = plan(home, root, adopt)
    if not changes:
        return profile_result(home, 'unchanged', version, before, changes)
    with exclusive_profile(home):
        version, before, changes = plan(home, root, adopt)
        if not changes:
            return profile_result(home, 'unchanged', version, before, changes)
        stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')
        backup = target(home, f'fstack/backups/profile-{stamp}')
        private_backup(backup)
        for name in changes:
            if before[name] is not None:
                copy = backup / name
                atomic_write(copy, before[name])
        receipt = {'version': version, 'files': {name: digest(before[name]) for name in changes if before[name] is not None},
                   'created': [name for name in changes if before[name] is None],
                   'removed': [name for name, content in changes.items() if content is None]}
        atomic_write(backup / 'manifest.json', (json.dumps(receipt, ensure_ascii=False, indent=2) + '\n').encode('utf-8'))
        written = []
        try:
            for name, content in changes.items():
                path = target(home, name)
                if read_optional(path) != before[name]:
                    raise ProfileConflict([name])
                if content is None:
                    path.unlink()
                else:
                    atomic_write(path, content)
                written.append(name)
        except Exception as original:
            failures = []
            for name in reversed(written):
                try:
                    path = target(home, name)
                    if read_optional(path) != changes[name]:
                        raise ProfileConflict([name])
                    if before[name] is None:
                        path.unlink()
                    else:
                        atomic_write(path, before[name])
                except Exception as error:
                    failures.append(error)
            if failures:
                raise ExceptionGroup('Ошибка синхронизации и восстановления; используйте резервную копию', [original, *failures])
            raise
    return {**profile_result(home, 'applied', version, before, changes), 'backup': str(backup)}


def main():
    parser = argparse.ArgumentParser(description='План и явная синхронизация пяти ролей и связей профиля fstack.')
    parser.add_argument('--home', type=Path, help='Целевой CODEX_HOME; по умолчанию переменная среды или ~/.codex')
    parser.add_argument('--apply', action='store_true', help='Применить план после проверки всех конфликтов')
    parser.add_argument('--adopt', action='store_true', help='С --apply принять отличающиеся неуправляемые файлы с резервной копией')
    args = parser.parse_args()
    if args.adopt and not args.apply:
        parser.error('--adopt допустим только вместе с --apply')
    home = (args.home or Path(os.environ.get('CODEX_HOME', Path.home() / '.codex'))).expanduser().resolve()
    try:
        if args.apply:
            result = apply_profile(home, adopt=args.adopt)
        else:
            version, before, changes = plan(home)
            result = profile_result(home, 'planned' if changes else 'unchanged', version, before, changes)
        print(json.dumps({'home': str(home), **result}, ensure_ascii=False, indent=2))
        return 0
    except ProfileConflict as error:
        print(json.dumps({'status': 'conflict', 'paths': error.paths}, ensure_ascii=False, indent=2))
        return 1
    except (OSError, ValueError, subprocess.CalledProcessError, ExceptionGroup) as error:
        result = {'status': 'error', 'message': str(error)}
        if isinstance(error, ExceptionGroup):
            result['causes'] = [str(cause) for cause in error.exceptions]
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 2


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    raise SystemExit(main())
