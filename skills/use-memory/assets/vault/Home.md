---
fstack_memory_home: 1
aliases:
  - Память Codex
---

# Память Codex

[[Codex Memory/Memory.canvas|Карта памяти]] · [[Codex Memory/Library.base|Таблица записей]] · [[Codex Memory/Templates/Review|Обзор записей]]

> [!abstract] Рабочий контекст между чатами
> Здесь хранятся решения, поправки и предпочтения для подключённых проектов. Открой страницу проекта, чтобы просмотреть сохранённое и разобрать Inbox.

## Проекты

```base
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
```

## Шаблоны

| Заметка | Когда подходит |
| --- | --- |
| [Решение](Templates/Decision.md) | Выбрали подход, который пригодится в следующих задачах |
| [Поправка](Templates/Correction.md) | Уточнили правило или исправили прежнее понимание |
| [Предпочтение](Templates/Preference.md) | Подтвердили устойчивый выбор в работе |
| [Обзор](Templates/Review.md) | Пора проверить Inbox и актуальность записей |

## Работа с памятью

Открой чат из рабочей папки проекта и попроси Codex "запомни это решение", "найди, что мы решили" или "покажи Inbox". Текст заметок можно править в Obsidian. Для подтверждения, замены и удаления используй навык памяти, чтобы изменение учитывалось при поиске на каждом устройстве.
