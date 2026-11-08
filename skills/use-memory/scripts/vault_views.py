import re
from pathlib import Path


HOME_MARKER = 'fstack_memory_home: 1'
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
LEGACY_FILTERS = (
    (f'{APPROVAL_FILE} != null', f'(note.fstack_status == "approved" || {APPROVAL_FILE} != null)'),
    (f'{APPROVAL_FILE} == null', f'(note.fstack_status != "approved" && {APPROVAL_FILE} == null)'),
)


def install_views(root, project, create_text):
    for relative in VAULT_FILES:
        create_text(root / relative, (ASSETS / relative).read_text(encoding='utf-8'))
    project_view = (ASSETS / 'Project.md').read_text(encoding='utf-8')
    create_text(root / 'Projects' / project / 'Project.md', project_view.replace('@PROJECT@', project))


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
        if read_text(path) != current:
            raise OSError('The memory library changed during the update')
        write_text(path, updated, replace=True)
