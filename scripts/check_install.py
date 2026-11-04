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

from check_package import ROOT, SKILLS, require


def command(binary, root, environment, *arguments):
    result = subprocess.run(
        [binary, '--disable', 'hooks', *arguments], cwd=root, env=environment,
        capture_output=True, text=True, encoding='utf-8', timeout=60,
    )
    if result.returncode:
        raise RuntimeError(f'{arguments}: {result.stderr}\n{result.stdout}')
    return json.loads(result.stdout)


def inspect(binary, root, home, package):
    environment = {**os.environ, 'CODEX_HOME': str(home)}
    for source in [*(root / 'skills').rglob('*'), *(root / 'assets').rglob('*')]:
        if source.is_file() and '__pycache__' not in source.parts:
            installed = package / source.relative_to(root)
            require(installed.is_file() and installed.read_bytes() == source.read_bytes(), f'Установка изменила {source}')
    process = subprocess.Popen(
        [binary, 'app-server', '--stdio'], cwd=root, env=environment,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, encoding='utf-8',
    )
    messages = queue.Queue()

    def receive():
        for line in process.stdout:
            try:
                messages.put(json.loads(line))
            except json.JSONDecodeError as error:
                messages.put(error)

    reader = threading.Thread(target=receive, daemon=True)
    reader.start()

    def request(identity, method, params):
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': params}) + '\n')
        process.stdin.flush()
        while True:
            response = messages.get(timeout=30)
            if isinstance(response, Exception):
                raise response
            if response.get('id') == identity:
                require('error' not in response, str(response))
                return response['result']

    try:
        request(1, 'initialize', {'clientInfo': {'name': 'fstack-install-check', 'version': '1.0'}, 'capabilities': {'experimentalApi': True}})
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'initialized'}) + '\n')
        process.stdin.flush()
        result = request(2, 'skills/list', {'cwds': [str(root)], 'forceReload': True})
        require(all(not item['errors'] for item in result['data']), str(result))
        skills = [s for item in result['data'] for s in item['skills'] if s['name'].startswith('fstack:')]
        require({s['name'] for s in skills} == {f'fstack:{name}' for name in SKILLS}, str(skills))
        require(all(s['enabled'] for s in skills), str(skills))
        for item in skills:
            require(Path(item['path']).is_relative_to(package), f'Навык загружен не из пакета: {item}')
            require(item['interface']['shortDescription'], f'Нет UI metadata: {item}')
        detail = request(3, 'plugin/read', {'pluginName': 'fstack', 'marketplacePath': str(root / '.agents/plugins/marketplace.json')})['plugin']
        require(len(detail['skills']) == len(SKILLS) and not detail['hooks'], 'Неожиданный состав плагина')
        require(Path(detail['summary']['interface']['logo']).is_file(), 'Логотип не разрешён клиентом')
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=10)
        reader.join(timeout=5)
        process.stdout.close()


def check(binary):
    with tempfile.TemporaryDirectory(prefix='fstack-') as temporary:
        base = Path(temporary).resolve()
        root = base / 'Marketplace source'
        home = base / 'Codex profile'
        home.mkdir()
        shutil.copytree(ROOT, root, ignore=shutil.ignore_patterns('.git', '.venv', '__pycache__', 'work', 'dist'))
        environment = {**os.environ, 'CODEX_HOME': str(home)}
        command(binary, root, environment, 'plugin', 'marketplace', 'add', str(root), '--json')
        result = command(binary, root, environment, 'plugin', 'add', 'fstack@fstack', '--json')
        require(result['pluginId'] == 'fstack@fstack', str(result))
        original = Path(result['installedPath'])
        inspect(binary, root, home, original)
        path = root / '.codex-plugin/plugin.json'
        manifest = json.loads(path.read_text(encoding='utf-8'))
        major, minor, patch = map(int, manifest['version'].split('.'))
        manifest['version'] = f'{major}.{minor}.{patch + 1}'
        path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        sample = root / 'skills/investigate-code/SKILL.md'
        sample.write_text(sample.read_text(encoding='utf-8') + '\n', encoding='utf-8')
        upgraded = command(binary, root, environment, 'plugin', 'add', 'fstack@fstack', '--json')
        require(Path(upgraded['installedPath']) != original, 'Новая версия не установлена отдельно')
        inspect(binary, root, home, Path(upgraded['installedPath']))
        print(f'Установка и обновление: {len(SKILLS)} навыков обнаружены из пакета, метаданные и ресурсы сохранены.')


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser()
    parser.add_argument('--codex', default='codex')
    args = parser.parse_args()
    binary = shutil.which(args.codex)
    if binary is None:
        raise FileNotFoundError(args.codex)
    check(binary)
