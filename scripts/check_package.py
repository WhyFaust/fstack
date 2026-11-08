import json
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import unquote

import yaml

from release import check_versions
from sync_profile import ROLES


ROOT = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def frontmatter(path):
    text = path.read_text(encoding='utf-8')
    parts = text.split('---', 2)
    require(len(parts) == 3 and not parts[0].strip(), f'{path}: отсутствует frontmatter')
    data = yaml.safe_load(parts[1])
    require(isinstance(data, dict), f'{path}: frontmatter должен быть объектом')
    return data


def contained_file(root, parent, relative, allow_directory=False):
    path = (parent / relative).resolve()
    require(path.is_relative_to(root.resolve()) and (path.is_file() or (allow_directory and path.is_dir())), f'Нет ресурса: {parent}/{relative}')
    return path


def markdown_targets(text):
    fence = None
    for line in text.splitlines():
        marker = re.match(r' {0,3}(`{3,}|~{3,})', line)
        if marker:
            token = marker.group(1)
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            continue
        if fence is not None:
            continue
        prose = re.sub(r'(`+).*?\1', '', line)
        yield from re.findall(r'\]\(([^)]+)\)', prose)


def skill_metadata(path):
    data = frontmatter(path)
    name = data.get('name')
    require(isinstance(name, str) and re.fullmatch(r'[a-z0-9]+(?:-[a-z0-9]+)*', name)
            and len(name) <= 64, f'{path}: некорректное имя навыка')
    require(name == path.parent.name, f'{path}: имя отличается от каталога')
    description = data.get('description')
    require(isinstance(description, str) and description.strip() and len(description) <= 1024,
            f'{path}: description должен содержать 1–1024 символа')
    for field in ('license', 'compatibility', 'allowed-tools'):
        if field in data or field == 'license':
            value = data.get(field)
            require(isinstance(value, str) and value.strip(), f'{path}: {field} должен быть непустой строкой')
    if 'compatibility' in data:
        require(len(data['compatibility']) <= 500, f'{path}: compatibility длиннее 500 символов')
    metadata = data.get('metadata')
    require(isinstance(metadata, dict) and all(isinstance(key, str) and isinstance(value, str)
            for key, value in metadata.items()), f'{path}: metadata должен быть строковым словарём')
    require(metadata.get('author') == 'WhyFaust'
            and metadata.get('source') == 'https://github.com/WhyFaust/fstack',
            f'{path}: отсутствуют автор или источник')
    return data


def skill_resources(folder, root):
    for name in ('scripts', 'references', 'assets'):
        directory = folder / name
        if directory.exists():
            contained_file(root, folder, name, allow_directory=True)
            require(directory.is_dir(), f'{directory}: ожидался каталог ресурсов')
            has_content = False
            for path in directory.rglob('*'):
                resource = contained_file(root, directory, path.relative_to(directory), allow_directory=True)
                if resource.is_file() and resource.stat().st_size and path.name not in {'.gitkeep', '.gitignore'}:
                    has_content = True
            require(has_content, f'{directory}: пустой каталог ресурсов')
    for path in folder.rglob('*.md'):
        for target in markdown_targets(path.read_text(encoding='utf-8')):
            target = target.split('#', 1)[0]
            if not target or '://' in target or target.startswith('mailto:'):
                continue
            contained_file(root, path.parent, unquote(target), allow_directory=True)


def role_settings(role, name):
    efforts = {'auditor': 'low', 'coder': 'medium', 'reviewer': 'high',
               'verifier': 'low', 'allure_analyst': 'medium'}
    require(role.get('model') == 'gpt-6.1-sol', f'{name}: некорректная модель роли')
    require(role.get('model_reasoning_effort') == efforts[name], f'{name}: некорректный reasoning роли')


def check_memory_hooks(root, relative):
    config = contained_file(root, root, relative)
    hooks = json.loads(config.read_text(encoding='utf-8'))['hooks']
    require(set(hooks) == {'SessionStart', 'UserPromptSubmit', 'PostToolUse', 'Stop'},
            'Неверный набор событий памяти')
    contained_file(root, root, 'skills/use-memory/scripts/memory.py')
    launchers = {
        'command': (r'/bin/sh "\$PLUGIN_ROOT/([^"]+)" (\w+)', 'hooks/memory.sh'),
        'commandWindows': (r'& "\$env:PLUGIN_ROOT\\([^"]+)" (\w+)', 'hooks/memory.ps1'),
    }
    for event, groups in hooks.items():
        require(len(groups) == 1 and len(groups[0]['hooks']) == 1, f'{event}: ожидалась одна команда памяти')
        if event == 'PostToolUse':
            require(groups[0].get('matcher') == '^Bash$', f'{event}: ожидался фильтр оболочки')
        handler = groups[0]['hooks'][0]
        require(handler['type'] == 'command', f'{event}: ожидался командный хук')
        for field, (pattern, relative_launcher) in launchers.items():
            target = re.fullmatch(pattern, handler[field])
            require(target and target[2] == event, f'{event}: неверная команда {field}')
            script = contained_file(root, root, target[1].replace('\\', '/'))
            require(script == contained_file(root, root, relative_launcher), f'{event}: команда запускает другой ресурс')


def check(root=ROOT):
    manifest = json.loads((root / '.codex-plugin/plugin.json').read_text(encoding='utf-8'))
    require(manifest['name'] == 'fstack', 'Некорректное имя плагина')
    require(re.fullmatch(r'\d+\.\d+\.\d+', manifest['version']), 'Версия должна быть SemVer')
    check_versions(root)
    require(manifest['skills'] == './skills/', 'Некорректный каталог навыков')
    extension = manifest.get('extensions', {}).get('com.openai', {})
    onboarding = extension.get('onboardingSkill')
    if onboarding:
        contained_file(root, root, onboarding)
    require(not manifest.get('hooks') and not manifest.get('mcpServers'), 'Неожиданная интеграция')
    check_memory_hooks(root, extension['hooks'])
    catalog = json.loads((root / '.agents/plugins/marketplace.json').read_text(encoding='utf-8'))
    require(catalog['name'] == 'fstack' and len(catalog['plugins']) == 1, 'Некорректный marketplace')
    entry = catalog['plugins'][0]
    require(entry['name'] == 'fstack' and entry['source'] == {'source': 'local', 'path': './'}, 'Некорректный source')
    skills = {path.name for path in (root / 'skills').iterdir() if path.is_dir()}
    listed = re.findall(r'^\| `([a-z0-9-]+)` \|', (root / 'docs/skills.md').read_text(encoding='utf-8'), re.M)
    require(skills and skills == set(listed) and len(listed) == len(skills), 'Каталог навыков не совпадает с пакетом')
    for name in sorted(skills):
        folder = root / 'skills' / name
        for path in folder.glob('SKILL*.md'):
            skill_metadata(path)
        require((folder / 'SKILL.md').is_file(), f'{name}: отсутствует SKILL.md')
        skill_resources(folder, root)
        ui = yaml.safe_load((folder / 'agents/openai.yaml').read_text(encoding='utf-8'))
        interface = ui['interface']
        require(25 <= len(interface['short_description']) <= 64, f'{name}: длина UI description')
        require(f'${name}' in interface['default_prompt'] or f'$fstack:{name}' in interface['default_prompt'], f'{name}: default_prompt без имени навыка')
        require(isinstance(ui.get('policy', {}).get('allow_implicit_invocation', True), bool), f'{name}: некорректная доступность')
        for field in ('icon_small', 'icon_large'):
            if interface.get(field):
                contained_file(folder, folder, interface[field])
    interface = manifest['interface']
    for relative in [interface['composerIcon'], interface['logo'], *interface.get('screenshots', [])]:
        asset = contained_file(root, root, relative)
        require(asset.read_bytes().startswith(b'\x89PNG\r\n\x1a\n'), f'{asset}: ожидался PNG')
    if manifest['name'] == 'fstack':
        roles = {path.stem: tomllib.loads(path.read_text(encoding='utf-8')) for path in (root / 'profile/agents').glob('*.toml')}
        require(set(roles) == set(ROLES), 'Набор ролей не совпадает с синхронизацией')
        for name, role in roles.items():
            require(role.get('name') == name and all(isinstance(role.get(field), str) and role[field].strip()
                    for field in ('description', 'developer_instructions')), f'Некорректная роль: {name}')
            role_settings(role, name)
            references = set(re.findall(r'fstack:([a-z0-9-]+)', role['developer_instructions']))
            require(references <= skills, f'{name}: отсутствуют навыки {references - skills}')
        config = tomllib.loads((root / 'profile/config.example.toml').read_text(encoding='utf-8'))
        agents = config['agents']
        require(isinstance(agents.get('enabled'), bool), 'agents.enabled должен быть boolean')
        limit = agents.get('max_concurrent_threads_per_session')
        require(type(limit) is int and limit > 0, 'Лимит подагентов должен быть положительным целым числом')
        efforts = {'low', 'medium', 'high', 'xhigh', 'max', 'ultra'}
        for settings, model, effort in ((config, 'model', 'model_reasoning_effort'),
                                         (agents, 'default_subagent_model', 'default_subagent_reasoning_effort')):
            require(settings.get(model) == 'gpt-6.1-sol', f'Некорректная модель примера: {model}')
            require(isinstance(settings.get(effort), str) and settings[effort] in efforts, f'Некорректный reasoning: {effort}')
        for relative in ('profile/AGENTS.md', 'scripts/sync_profile.py'):
            contained_file(root, root, relative)
    for path in root.rglob('*.md'):
        if any(part in {'.git', '.venv', 'work'} for part in path.relative_to(root).parts):
            continue
        text = path.read_text(encoding='utf-8')
        references = set(re.findall(r'\bfstack:([a-z][a-z0-9-]*)', text)) - {'profile'}
        require(references <= skills, f'{path}: отсутствуют навыки {references - skills}')
        for target in markdown_targets(text):
            target = target.split('#', 1)[0]
            if not target or '://' in target or target.startswith('mailto:'):
                continue
            contained_file(root, path.parent, unquote(target), allow_directory=True)
    print(f'Пакет {manifest["version"]}: {len(skills)} навыков, метаданные, ссылки и ресурсы проверены.')


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    check()
