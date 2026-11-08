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


def install_views(root, project, create_text):
    for relative in VAULT_FILES:
        create_text(root / relative, (ASSETS / relative).read_text(encoding='utf-8'))
    project_view = (ASSETS / 'Project.md').read_text(encoding='utf-8')
    create_text(root / 'Projects' / project / 'Project.md', project_view.replace('@PROJECT@', project))
