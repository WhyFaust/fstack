import argparse
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

from check_package import skill_resources
from release import check_versions
from sync_profile import ROLES, STATE, digest, sources


ROOT = Path(__file__).resolve().parents[1]


def run(executable, arguments, environment, directory):
    result = subprocess.run([executable, *arguments], env=environment, cwd=directory,
                            text=True, encoding='utf-8', capture_output=True, timeout=45)
    if result.returncode:
        raise RuntimeError(f'{arguments[0]}: {result.stderr or result.stdout}')
    return result.stdout


def discover_package(executable, environment, directory):
    with (directory / 'app-server.log').open('w', encoding='utf-8') as errors:
        process = subprocess.Popen([executable, 'app-server', '--stdio', '--strict-config', '--disable', 'apps',
                                    '--disable', 'remote_plugin', '-c', 'check_for_update_on_startup=false'], env=environment,
                                   cwd=directory, text=True, encoding='utf-8', stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=errors)
        messages = queue.Queue()

        def read():
            try:
                for line in process.stdout:
                    messages.put(json.loads(line))
            except (OSError, ValueError) as error:
                messages.put(error)
            finally:
                messages.put(None)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()

        def send(message):
            process.stdin.write(json.dumps(message) + '\n')
            process.stdin.flush()

        def request(identifier, method, parameters):
            send({'id': identifier, 'method': method, 'params': parameters})
            deadline = time.monotonic() + 30
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f'Нет ответа app-server: {method}')
                message = messages.get(timeout=remaining)
                if message is None or isinstance(message, Exception):
                    raise RuntimeError(f'Остановлен поток app-server: {message}')
                if message.get('id') == identifier:
                    if 'error' in message:
                        raise RuntimeError(str(message['error']))
                    return message['result']

        try:
            request(1, 'initialize', {'clientInfo': {'name': 'fstack-installation-check', 'version': '1.0'},
                                    'capabilities': {'experimentalApi': True, 'explicitGatewayOauth': True}})
            send({'method': 'initialized'})
            cwds = [str(directory)]
            skills = request(2, 'skills/list', {'cwds': cwds, 'forceReload': True})['data']
            hooks = request(3, 'hooks/list', {'cwds': cwds})['data']
            return skills, hooks
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            process.stdin.close()
            reader.join(timeout=5)
            process.stdout.close()


def check(executable, root=ROOT):
    version = check_versions(root)
    expected = {'fstack:' + path.parent.name for path in (root / 'skills').glob('*/SKILL.md')}
    with tempfile.TemporaryDirectory(prefix='fstack-install-') as temporary:
        directory = Path(temporary)
        home = directory / 'codex'
        home.mkdir()
        shutil.copyfile(root / 'profile/config.example.toml', home / 'config.toml')
        environment = {**os.environ, 'CODEX_HOME': str(home)}
        client_version = run(executable, ['--version'], environment, directory).strip()
        run(executable, ['plugin', 'marketplace', 'add', str(root)], environment, directory)
        installed = json.loads(run(executable, ['plugin', 'add', 'fstack@fstack', '--json'], environment, directory))
        installed_path = Path(installed['installedPath']).resolve()
        if not installed_path.is_relative_to(home.resolve()) or installed['version'] != version:
            raise ValueError('Установка не соответствует временному профилю и версии пакета')
        listing = json.loads(run(executable, ['plugin', 'list', '--json'], environment, directory))
        if not any(item['pluginId'] == 'fstack@fstack' and item['enabled'] and item['installed']
                   for item in listing['installed']):
            raise ValueError('Клиент не подтвердил включённый установленный плагин')
        for folder in (installed_path / 'skills').iterdir():
            if folder.is_dir():
                skill_resources(folder, installed_path)
        helper = str(installed_path / 'scripts/sync_profile.py')
        run(sys.executable, [helper, '--home', str(home), '--apply'], environment, directory)
        repeat = json.loads(run(sys.executable, [helper, '--home', str(home)], environment, directory))
        if repeat['status'] != 'unchanged':
            raise ValueError('Повторная синхронизация меняет профиль')
        _, payloads, _ = sources(installed_path)
        installed_roles = {path.stem for path in (home / 'agents').glob('*.toml')}
        state = json.loads((home / STATE).read_text(encoding='utf-8'))
        if installed_roles != set(ROLES) or set(state['files']) != set(payloads):
            raise ValueError('Установленные роли или состояние не совпадают с пакетом')
        for name, content in payloads.items():
            if (home / name).read_bytes() != content or state['files'][name] != digest(content):
                raise ValueError(f'Установленная роль отличается от источника: {name}')
        entries, hook_entries = discover_package(executable, environment, directory)
        errors = [error for entry in entries for error in entry['errors']]
        skills = [skill for entry in entries for skill in entry['skills'] if skill.get('pluginId') == 'fstack@fstack']
        if errors or {skill['name'] for skill in skills} != expected or not all(skill['enabled'] for skill in skills):
            raise ValueError(f'Обнаружение навыков не совпадает с пакетом: {errors}')
        for skill in skills:
            if '$' + skill['name'] not in skill['interface']['defaultPrompt']:
                raise ValueError(f'UI-подсказка использует другое имя: {skill["name"]}')
        hook_errors = [error for entry in hook_entries for error in entry['errors']]
        hooks = [hook for entry in hook_entries for hook in entry['hooks'] if hook.get('pluginId') == 'fstack@fstack']
        events = {'sessionStart', 'userPromptSubmit', 'postToolUse', 'stop'}
        if hook_errors or len(hooks) != len(events) or {hook['eventName'] for hook in hooks} != events:
            raise ValueError(f'Обнаружение хуков памяти не совпадает с пакетом: {hook_errors}')
        if any(hook['source'] != 'plugin' or hook['handlerType'] != 'command'
               or hook['trustStatus'] != 'untrusted' or not Path(hook['sourcePath']).resolve().is_relative_to(installed_path)
               for hook in hooks):
            raise ValueError('Источник или состояние доверия установленных хуков памяти отличаются от ожидаемых')
    return {'status': 'passed', 'client': client_version, 'version': version,
            'skills_discovered': len(expected), 'hooks_discovered': sorted(events), 'profile_roundtrip': 'unchanged',
            'roles_installed': sorted(installed_roles), 'model_availability': 'not_checked',
            'model_tasks': 'not_run', 'role_execution': 'not_run'}


def main():
    parser = argparse.ArgumentParser(description='Изолированная проверка установки и обнаружения без задач модели.')
    parser.add_argument('--codex', default='codex', help='Установленный Codex; ничего не скачивается этим скриптом')
    args = parser.parse_args()
    executable = shutil.which(args.codex)
    if executable is None:
        parser.exit(2, 'Codex не найден; проверка установки заблокирована.\n')
    try:
        print(json.dumps(check(executable), ensure_ascii=False, indent=2))
    except (OSError, RuntimeError, ValueError, KeyError, queue.Empty, subprocess.TimeoutExpired) as error:
        parser.exit(1, f'Проверка установки не пройдена: {error}\n')


if __name__ == '__main__':
    main()
