import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

import yaml


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = Path('.codex-plugin/plugin.json')
SEMVER = re.compile(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)')


def run(*args):
    result = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, encoding='utf-8', check=True)
    return result.stdout.strip()


def version_tuple(value):
    match = SEMVER.fullmatch(value)
    if match is None:
        raise ValueError(f'Некорректная версия: {value}')
    return tuple(map(int, match.groups()))


def increment(version, level):
    major, minor, patch = version_tuple(version)
    return (version, f'{major}.{minor}.{patch + 1}', f'{major}.{minor + 1}.0', f'{major + 1}.0.0')[level]


def change_level(messages):
    level = 0
    for message in messages:
        header = re.match(r'([a-zA-Z]+)(?:\([^\r\n)]+\))?(!)?:[ \t]+\S', message.strip())
        breaking = (header and header[2]) or re.search(r'^BREAKING(?: |-)CHANGE:[ \t]+\S', message, re.M)
        kind = header[1].lower() if header else None
        level = max(level, 3 if breaking else 2 if kind == 'feat' else 1 if kind in {'fix', 'perf'} else 0)
    return level


def manifest(root=ROOT):
    return json.loads((root / MANIFEST).read_text(encoding='utf-8'))


def skill_documents(root=ROOT):
    result = []
    for path in sorted((root / 'skills').glob('*/SKILL*.md')):
        text = path.read_text(encoding='utf-8')
        match = re.match(r'---\n(.*?)\n---(?:\n|$)', text, re.S)
        if match is None:
            raise ValueError(f'Нет frontmatter: {path}')
        data = yaml.safe_load(match[1])
        result.append((path, text, match, data))
    if not result:
        raise ValueError('Не найдены навыки')
    return result


def check_versions(root=ROOT):
    current = manifest(root)['version']
    version_tuple(current)
    for path, _, _, data in skill_documents(root):
        if data.get('metadata', {}).get('version') != current:
            raise ValueError(f'Версия навыка не совпадает с плагином: {path}')
    return current


def set_version(version):
    version_tuple(version)
    data = manifest()
    data['version'] = version
    changes = {ROOT / MANIFEST: json.dumps(data, ensure_ascii=False, indent=2) + '\n'}
    for path, text, match, data in skill_documents():
        metadata = data.setdefault('metadata', {})
        if not isinstance(metadata, dict):
            raise ValueError(f'Некорректные metadata: {path}')
        metadata['version'] = version
        header = yaml.safe_dump(data, allow_unicode=True, sort_keys=False, width=100).rstrip()
        changes[path] = f'---\n{header}\n---\n{text[match.end():]}'
    for path, text in changes.items():
        path.write_text(text, encoding='utf-8', newline='\n')


def release_records(repository):
    pages = json.loads(run('gh', 'api', '--paginate', '--slurp', f'repos/{repository}/releases?per_page=100'))
    return [release for page in pages for release in page]


def plan(repository):
    records = release_records(repository)
    stable = [r for r in records if not r['draft'] and not r['prerelease'] and
              r['tag_name'].startswith('v') and SEMVER.fullmatch(r['tag_name'][1:])]
    if stable:
        latest = max(stable, key=lambda r: version_tuple(r['tag_name'][1:]))
        base, version = latest['tag_name'], latest['tag_name'][1:]
        run('git', 'merge-base', '--is-ancestor', base, 'HEAD')
    else:
        additions = run('git', 'log', '--diff-filter=A', '--format=%H', '--', '.github/workflows/release.yml').splitlines()
        if not additions:
            raise ValueError('Сначала закоммитьте workflow релиза')
        base = run('git', 'rev-parse', additions[-1] + '^')
        version = json.loads(run('git', 'show', f'{base}:{MANIFEST.as_posix()}'))['version']
    messages = run('git', 'log', '--format=%B%x00', f'{base}..HEAD').split('\0')
    level = change_level(messages)
    current = check_versions()
    candidates = {increment(version, n) for n in range(level + 1)}
    if current not in candidates:
        raise ValueError(f'Версия {current} не соответствует опубликованной или ожидающей версии {sorted(candidates)}')
    return {'version': increment(version, level), 'level': level, 'base': base, 'records': records}


def archive(version, revision='HEAD'):
    name = manifest()['name']
    output = ROOT / 'dist' / f'{name}-{version}.zip'
    output.parent.mkdir(exist_ok=True)
    run('git', '-c', 'core.autocrlf=false', 'archive', '--format=zip', f'--prefix={name}/', f'--output={output}', revision)
    checksum = output.with_suffix('.zip.sha256')
    checksum.write_text(f'{hashlib.sha256(output.read_bytes()).hexdigest()}  {output.name}\n', encoding='utf-8')
    return output, checksum


def finish_release(repository, version, records, base, sha=None):
    tag = 'v' + version
    sha = sha or run('git', 'rev-parse', 'HEAD')
    marker = f'<!-- release-source:{sha} -->'
    existing = next((r for r in records if r['tag_name'] == tag), None)
    tags = run('git', 'tag', '--list', tag)
    if tags and run('git', 'rev-parse', f'{tag}^{{commit}}') != sha:
        raise ValueError(f'Тег {tag} указывает на другую ревизию')
    if existing:
        if not existing['draft']:
            raise ValueError(f'Релиз {tag} уже опубликован; проверьте его ревизию')
        if marker not in (existing.get('body') or '') or existing['author']['login'] != 'github-actions[bot]':
            raise ValueError(f'Черновик {tag} не принадлежит этому выпуску')
    output, checksum = archive(version, sha)
    if not existing:
        entries = run('git', 'log', '--format=- %s (%h)', f'{base}..HEAD')
        notes = ROOT / 'dist' / 'release-notes.md'
        notes.write_text(f'## Изменения\n\n{entries}\n\n{marker}\n', encoding='utf-8')
        run('gh', 'release', 'create', tag, '--repo', repository, '--target', sha, '--title', tag,
            '--notes-file', str(notes), '--draft')
    run('gh', 'release', 'upload', tag, str(output), str(checksum), '--repo', repository, '--clobber')
    run('gh', 'release', 'edit', tag, '--repo', repository, '--draft=false', '--latest')
    print(json.dumps({'published': tag, 'sha': sha}, ensure_ascii=False))


def publish(repository):
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Публикация выполняется только workflow ветки main')
    if os.environ.get('GITHUB_REPOSITORY') != repository:
        raise ValueError('Репозиторий workflow не совпадает с целью')
    if run('git', 'status', '--porcelain'):
        raise ValueError('Рабочее дерево содержит изменения')
    for record in release_records(repository):
        if not record['draft'] or record['author']['login'] != 'github-actions[bot]':
            continue
        marker = re.search(r'^<!-- release-source:([a-f0-9]{40}) -->$', record.get('body') or '', re.M)
        if marker is None:
            continue
        sha = marker[1]
        version = record['tag_name'].removeprefix('v')
        version_tuple(version)
        run('git', 'merge-base', '--is-ancestor', sha, 'HEAD')
        saved = json.loads(run('git', 'show', f'{sha}:{MANIFEST.as_posix()}'))
        if saved['version'] != version:
            raise ValueError('Версия черновика не совпадает с манифестом его коммита')
        finish_release(repository, version, [record], sha + '^', sha)
        run('git', 'fetch', 'origin', 'tag', record['tag_name'])
    proposed = plan(repository)
    if not proposed['level']:
        print('Нет изменений, требующих релиза.')
        return
    set_version(proposed['version'])
    run(sys.executable, 'scripts/check_package.py')
    run('git', 'config', 'user.name', 'github-actions[bot]')
    run('git', 'config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com')
    files = [str(MANIFEST), *[str(p.relative_to(ROOT)) for p, _, _, _ in skill_documents()]]
    run('git', 'add', '--', *files)
    if run('git', 'diff', '--cached', '--name-only'):
        run('git', 'commit', '-m', f'chore(release): v{proposed["version"]}')
    run('git', 'push', 'origin', 'HEAD:main')
    finish_release(repository, proposed['version'], proposed['records'], proposed['base'])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['check', 'plan', 'publish'])
    args = parser.parse_args()
    if args.action == 'check':
        print(check_versions())
        return
    repository = os.environ.get('GH_REPO') or run('gh', 'repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner')
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', repository):
        raise ValueError('Некорректное имя репозитория')
    if args.action == 'publish':
        publish(repository)
    else:
        proposed = plan(repository)
        print(json.dumps({k: v for k, v in proposed.items() if k != 'records'}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stderr.reconfigure(encoding='utf-8')
    main()
