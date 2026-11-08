import re
from pathlib import Path


HOME_MARKER = 'fstack_memory_home: 1'
PROJECTS_MARKER = 'fstack_memory_projects: 1'
ASSETS = Path(__file__).resolve().parents[1] / 'assets' / 'vault'
VAULT_FILES = (
    'Home.md',
    'Memory.canvas',
    'Library.base',
    'Templates/Decision.md',
    'Templates/Correction.md',
    'Templates/Preference.md',
    'Templates/Review.md',
)
APPROVAL_FILE = 'file(file.path.replace("/Records/", "/Approved/"))'
LEGACY_FORGOTTEN_FILE = 'file(file.path.replace("/Records/", "/Forgotten/"))'
FORGOTTEN_FILE = 'file(file.folder + "/" + file.basename + "-forgotten.json")'
PROJECTS_LINK = '[[Codex Memory/Projects|Все проекты]]'
LEGACY_PROJECTS_LINK = '[[Codex Memory/Home|Все проекты]]'
LEGACY_HOME_BASE = """```base
filters:
  and:
    - 'file.inFolder("Codex Memory/Projects")'
    - 'file.path.endsWith("/Project.md")'
    - 'note.fstack_memory_project != null'
formulas:
  project: 'link(file.path, note.fstack_memory_project)'
views:
  - type: list
    name: Проекты
    order: [formula.project]
```"""
LEGACY_FILTERS = (
    (f'{APPROVAL_FILE} != null', f'(note.fstack_status == "approved" || {APPROVAL_FILE} != null)'),
    (f'{APPROVAL_FILE} == null', f'(note.fstack_status != "approved" && {APPROVAL_FILE} == null)'),
    (f'{LEGACY_FORGOTTEN_FILE} == null', f'({FORGOTTEN_FILE} == null && {LEGACY_FORGOTTEN_FILE} == null)'),
    ('!this.file.path.endsWith("/Project.md") || file.inFolder(this.file.folder)',
     'this.file.properties.fstack_memory_project == null || '
     'file.folder == "Codex Memory/Projects/" + this.file.properties.fstack_memory_project + "/Records"'),
)


def install_views(root, create_text):
    for relative in VAULT_FILES:
        create_text(root / relative, (ASSETS / relative).read_text(encoding='utf-8'))


def project_template(project):
    return (ASSETS / 'Project.md').read_text(encoding='utf-8').replace('@PROJECT@', project)


def named_project_text(legacy):
    metadata, delimiter, body = legacy[4:].partition('\n---\n')
    stock_navigation = f'{LEGACY_PROJECTS_LINK} · [[Codex Memory/Templates/Review|Шаблон обзора]]'
    pattern = rf'(?m)^{re.escape(stock_navigation)}$'
    body, replaced = re.subn(pattern, stock_navigation.replace(LEGACY_PROJECTS_LINK, PROJECTS_LINK), body, count=1)
    if not replaced and PROJECTS_LINK not in body:
        body = f'\n{PROJECTS_LINK}\n' + body
    return '---\n' + metadata + delimiter + body


def is_projects_hub(text):
    if text is None or not text.startswith('---\n'):
        return False
    metadata, delimiter, _ = text[4:].partition('\n---\n')
    return bool(delimiter and PROJECTS_MARKER in metadata.splitlines())


def ensure_projects_hub(root, read_text, write_text):
    hub = root / 'Projects.md'
    current = read_text(hub)
    if current is None:
        write_text(hub, (ASSETS / hub.name).read_text(encoding='utf-8'))
        current = read_text(hub)
    if not is_projects_hub(current):
        raise OSError('Memory projects hub is occupied')

    home = root / 'Home.md'
    current = read_text(home)
    if current is None:
        raise OSError('Memory home is unavailable')
    if LEGACY_HOME_BASE in current:
        updated = current.replace(LEGACY_HOME_BASE, PROJECTS_LINK, 1)
    elif PROJECTS_LINK not in current:
        updated = current + f'\n\n{PROJECTS_LINK}\n'
    else:
        return
    write_text(home, updated, replace=True, expected=current)


def ensure_library(root, read_text, write_text):
    path = root / 'Library.base'
    current = read_text(path)
    if current is None:
        write_text(path, (ASSETS / path.name).read_text(encoding='utf-8'))
        current = read_text(path)
        if current is None:
            raise OSError('Cannot initialize the memory library')

    updated = current
    for legacy, replacement in LEGACY_FILTERS:
        pattern = rf"(?m)^([ \t]*(?:filters:|-)[ \t]+)('?)" rf"{re.escape(legacy)}\2([ \t]*(?:#[^\n]*)?)$"
        updated = re.sub(pattern, rf"\g<1>\g<2>{replacement}\g<2>\g<3>", updated)
    if updated != current:
        write_text(path, updated, replace=True, expected=current)
