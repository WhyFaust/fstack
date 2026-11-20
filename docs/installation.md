# Установка и обновление

## Через маркетплейс

Нужны Git, системный `curl` и Codex с поддержкой плагинов и фоновых хуков.
Платформы — Linux x64 (glibc 2.35+), Windows x64 и macOS Intel/Apple Silicon.

```bash
codex plugin marketplace add WhyFaust/fstack --ref main
codex plugin add fstack@fstack --json
```

Одобри хуки и открой чат. При старте fstack скачивает бинарник своей версии
и платформы, проверяет SHA-256 и сохраняет его в кэше профиля. При сбое
посмотри журнал `SessionStart`. Если выпуск ещё загружается, повтори запуск позже.

Для автономной установки скачай ZIP платформы и `.zip.sha256` из
[выпуска](https://github.com/WhyFaust/fstack/releases). Сверь сумму, распакуй
архив в постоянный каталог и зарегистрируй его через
`codex plugin marketplace add <каталог>`. Затем выполни `codex plugin add fstack@fstack`.
Для установки из клона сначала [собери бинарник](development.md#минимальные-проверки).

## Диагностика установки

В каталоге проекта запусти `fstack doctor`. Отчёт покажет активную версию,
пути, число навыков и доверие к хукам. Если команды нет в `PATH`, запусти
`bin/fstack` из ZIP (в Windows — `bin\fstack.exe`).

```bash
fstack doctor --codex /absolute/path/to/codex --json
```

Код выхода `0` означает успех, `1` — ошибки в поле `issues`.
Профиль задаёт `CODEX_HOME`.

## Обновление

Клиент с автообновлением получает marketplace при старте. Для ручного обновления:

```bash
codex plugin marketplace upgrade fstack --json
codex plugin list --json
```

Если `enabled: true`, выполни `codex plugin add fstack@fstack --json`, сверь
`version` и `installedPath`, одобри изменившиеся хуки и открой новый чат. Для
выключенного плагина отложи установку до его включения. Пустой `upgradedRoots`
означает, что marketplace уже обновлён.
При автономной установке распакуй новый ZIP отдельно и переключи регистрацию.

## Настройки подагентов

Полная история передаёт подагенту параметры родителя. Для свежего контекста
можно выбрать поддерживаемые модель и глубину. [Выбор исполнителя](../skills/delegate/SKILL.md#выбрать-исполнителя).

`agents.max_concurrent_threads_per_session` ограничивает число подагентов на чат.
Укажи положительный предел или удали поле для значения клиента по умолчанию.
Проверь слоты в новом чате. [Настройки Codex](https://learn.chatgpt.com/docs/agent-configuration/subagents).

## Сверить установленный снимок

В каталоге `installedPath` выполни команду своей платформы.

```bash
/bin/sh launchers/fstack.sh check-package .
```

```powershell
& .\launchers\fstack.ps1 check-package .
```

Для ручной загрузки бинарника передай запускателю `--install-runtime`
(в PowerShell — `-InstallRuntime`).

## Удаление

`codex plugin remove fstack@fstack` удаляет плагин,
`codex plugin marketplace remove fstack` — регистрацию источника.
Заметки Obsidian сохраняются.
