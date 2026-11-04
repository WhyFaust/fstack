<div align="center">

<img src="assets/fstack.png" width="120" alt="Маскот Faust stack">

# fstack

**Faust stack · личная инженерная среда для Codex**

Понять задачу. Сделать нужное. Проверить результат.

[Начать](docs/installation.md) · [Codex и агенты](docs/codex.md) · [Навыки](docs/skills.md) · [Архитектура](docs/architecture.md) · [Разработка](docs/development.md)

<img src="assets/cover.png" width="900" alt="Faust stack — мастерская инженерных навыков">

</div>

## Один плагин, тринадцать навыков и пять агентов

fstack собирает личные навыки в версионируемый пакет. Codex видит их описания,
выбирает подходящий навык и загружает подробности по необходимости.
Установка и обновления идут через собственный маркетплейс.

| Направление | Навыки |
| --- | --- |
| Понять | `investigate-code`, `find-docs` |
| Изменить | `implement-change`, `debug-failure`, `refactor-code` |
| Проверить | `review-change`, `verify-result` |
| Выбрать решение | `engineering-principles` |
| Работать с инструментами | `python-aqa`, `glab`, `playwright-cli` |
| Сравнить настройки | `codex-config-evaluation` |
| Подключить профиль | `fstack-setup` |

Это набор отдельных методов. Задача определяет нужный метод; проходить все навыки
по очереди не требуется.

Роли `auditor`, `coder`, `python_coder`, `reviewer` и `allure_analyst` используют
эти навыки и подходящие установленные провайдеры. [Руководство Codex](docs/codex.md)
содержит пример `config.toml`, шаблон `AGENTS.md` и безопасную синхронизацию ролей.

## Подключение

Для доступа к приватному репозиторию Git должен быть авторизован в GitHub.

```bash
codex plugin marketplace add WhyFaust/fstack --ref main
codex plugin add fstack@fstack
```

Начни новый чат. Для явного выбора используй, например:

```text
Используй $fstack:investigate-code и объясни, кто владеет состоянием этой операции.
Используй $fstack:review-change и проверь изменения в указанной ревизии.
```

## Обновление

```bash
codex plugin marketplace upgrade fstack
codex plugin add fstack@fstack
```

После обновления начни новый чат. Свои изменения вноси в этот репозиторий:
установленный кэш заменяется при обновлении.

После обновления плагина синхронизируй роли через `$fstack:fstack-setup`.
Версии плагина и навыков повышаются автоматически по Conventional Commits;
ZIP-пакеты и контрольные суммы публикуются в [релизах](https://github.com/WhyFaust/fstack/releases).

Подробнее: [установка и восстановление](docs/installation.md),
[выбор навыка](docs/skills.md), [подготовка новой версии](docs/development.md).

При таймауте Git-обновления в текущем Windows-клиенте используй
[маркетплейс из локального Git-клона](docs/installation.md#если-git-обновление-в-codex-прерывается-по-таймауту).
