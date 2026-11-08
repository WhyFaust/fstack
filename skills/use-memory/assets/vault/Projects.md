---
fstack_memory_projects: 1
---

# Проекты

[[Codex Memory/Home|Память Codex]]

```base
filters:
  and:
    - 'file.inFolder("Codex Memory/Projects")'
    - 'note.fstack_memory_project != null'
    - or:
        - 'file.path == formula.named_path'
        - and:
            - 'file.path == "Codex Memory/Projects/" + note.fstack_memory_project + "/Project.md"'
            - 'if(file(formula.named_path) != null, file(formula.named_path).properties.fstack_memory_project != note.fstack_memory_project, true)'
formulas:
  named_path: '"Codex Memory/Projects/" + note.fstack_memory_project + "/" + if(note.fstack_memory_project == "project", "Project", note.fstack_memory_project) + ".md"'
  project: 'link(file.path, note.fstack_memory_project)'
views:
  - type: list
    name: Проекты
    order: [formula.project]
```
