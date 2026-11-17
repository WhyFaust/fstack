# Разработка и выпуск

## Минимальные проверки

Для разработки нужны Rust 1.90.0 и системный C/C++ toolchain для сборки SQLite.
На Windows имя бинарника — `fstack.exe`. Из корня репозитория выполни:

```bash
rustup toolchain install 1.90.0 --profile minimal --component rustfmt,clippy
cargo +1.90.0 build --release --locked --manifest-path tools/fstack/Cargo.toml
cargo +1.90.0 fmt --manifest-path tools/fstack/Cargo.toml -- --check
cargo +1.90.0 clippy --all-targets --locked --manifest-path tools/fstack/Cargo.toml -- -D warnings
cargo +1.90.0 test --release --locked --manifest-path tools/fstack/Cargo.toml
mkdir -p bin
cp tools/fstack/target/release/fstack bin/fstack
bin/fstack --package . check-package .
bin/fstack --package . release check
```

CI проверяет сборку на Linux x64, Windows x64 и macOS на Intel и Apple
Silicon. Список платформ, флаги сборки и проверяемые зависимости определены
в [workflow](../.github/workflows/release.yml). Для тестов памяти используй
временные vault и профиль вне Git-дерева.

`check-package` проверяет локальные Markdown-ссылки в инструкциях и документации.
Для ссылок с `#` он ищет заголовок или HTML-якорь в целевом Markdown-файле.
При ошибке выводит файл, ссылку и отсутствующий раздел.

В Linux CI работают Python-проверки измерителя и runner без моделей.

```bash
python -m unittest discover -s tools/fstack/benchmarks -p 'test_*.py' -v
python -m unittest discover -s tools/fstack/evals -p 'test_*.py' -v
```

## Проверка фактическим клиентом

```bash
bin/fstack check-installation --codex /absolute/path/to/codex
python tools/fstack/evals/check_doctor.py /absolute/path/to/package --codex /absolute/path/to/codex
python tools/fstack/evals/check_skill_invocation.py /absolute/path/to/package --codex /absolute/path/to/codex
```

Команда устанавливает пакет в отдельный временный профиль. Она сверяет
версию и содержимое навыков, обнаружение хуков и загрузку конфигурации.
`check_doctor.py` проверяет отчёт и код выхода до установки, после установки
и после одобрения хуков. `check_skill_invocation.py` направляет два запроса
Codex в локальный HTTP-обработчик. Явный вызов `$fstack:review` должен передать
полный текст навыка, обычный запрос пропускает его. В Linux CI эти проверки
выполняются с закреплённой версией CLI из workflow. Выполнение задач моделью
проверяется отдельно.

Для проверки записи в Codex Desktop отправь поправку из поля ввода нового
чата проекта в обычном режиме. Убедись, что `UserPromptSubmit` выдал команду
`memory stage`, а после завершения хода `memory search` находит заметку
в Obsidian с `session_id` этого чата. В проверенном Codex 0.169.0-alpha.11.1
поручения `create_thread` и `send_message_to_thread` приходят как результаты
инструментов. `UserPromptSubmit` запускается при пользовательском вводе.
[Обработка входа в Codex](https://github.com/openai/codex/blob/rust-v0.169.0-alpha.11/codex-rs/core/src/hook_runtime.rs#L708-L748).

Для отдельного чата Codex и нативного подагента проверь раннюю подсказку поиска,
запрос итога на первом `Stop` или `SubagentStop`, приём `memory report`
и сохранение на следующем событии завершения. Для прерванного и запланированного
хода проверь отсутствие записи. Runtime читает происхождение и действительный
режим по локальному журналу, который передал клиент. При смене формата повтори
проверку с журналом нового клиента. [Контракт журнала в хуках](https://learn.chatgpt.com/docs/hooks#common-input-fields).

## Изменение навыка

Следуй [skill](../skills/skill/SKILL.md). Обнови
[каталог](skills.md) при добавлении, переименовании или удалении навыка.
В шапке `SKILL.md` достаточно имени, описания и запрета неявного вызова.
После шапки напиши инструкцию в форме, подходящей задаче. Храни лицензию
и атрибуцию рядом с источниками.

## Версии и публикация

Используй [шаблон PR](../.github/PULL_REQUEST_TEMPLATE.md). Причина, выбор
решения и фактическая проверка должны быть понятны без истории чата.
Порядок заполнения и запасной шаблон для других проектов описаны в
[implement](../skills/implement/SKILL.md#подготовить-pr-или-mr).

Выбери версию пакета по характеру изменения. Исправление повышает PATCH,
новая возможность или несовместимый контракт в серии 0.x повышает MINOR.
MAJOR меняется по прямому указанию пользователя. Для него измени значение
в [launchers/package-major](../launchers/package-major).

Добавь входы сборки в индекс Git и подготовь версию и идентификатор сборки.

```bash
bin/fstack --package . release prepare VERSION
git add .codex-plugin/plugin.json launchers/package-version launchers/runtime-id
```

Уже выбранную версию можно оставить при пересчёте идентификатора сборки
командой `release prepare` без аргумента. Включи результат в PR. CI сверяет
снимок с базой PR. Новая версия нужна при изменении навыков, каталога,
манифеста, ресурсов интерфейса, регистрации marketplace, лицензий или входов
сборки. Перед слиянием CI проверяет номер по точному имени в GitHub Releases,
включая выпуск с удалённым Git-тегом. В merge queue те же проверки выполняются
на коммите события `merge_group`.

После слияния проверь выпуск из `main`. Workflow сверяет SHA, использует
артефакты проверенных сборок и публикует архивы, бинарники и SHA-256.
В разделе «При обновлении» PR, который меняет версию, укажи действия пользователя.
Издатель переносит этот раздел в описание нового GitHub Release. Если переход
не требует действий, раздел можно удалить. Команда `release plan` показывает
план публикации для версии из манифеста.
При временном сбое повтор продолжает принадлежащий выпуску черновик
с той же проверенной версией и исходниками.
