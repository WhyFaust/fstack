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

## Проверка фактическим клиентом

```bash
bin/fstack check-installation --codex /absolute/path/to/codex
```

Команда устанавливает пакет в отдельный временный профиль. Она сверяет
версию и содержимое навыков, обнаружение хуков и загрузку конфигурации.
В отчёте указывай версию клиента. Выполнение задач моделью проверяется отдельно.

## Изменение навыка

Следуй [adapt-skill](../skills/adapt-skill/SKILL.md). Обнови
[каталог](skills.md) при добавлении, переименовании или удалении навыка.
Повышай `metadata.version` только у содержательно изменённых навыков,
согласованно во всех локализациях. Новый навык начинает с `1.0.0`.

## Версии и публикация

Ветка PR должна учитывать актуальную `main`. Используй Conventional Commits:
`fix:` и `perf:` повышают PATCH, `feat:` — MINOR, несовместимый публичный
контракт с `!` или `BREAKING CHANGE:` — MAJOR. Чисто редакционные изменения
могут сохранить версию. Изменение поведения навыка или runtime требует релиза.

Добавь изменённые входы сборки в индекс Git. Задай в `PR_TITLE` заголовок PR и выполни:

```bash
bin/fstack --package . release prepare origin/main "$PR_TITLE"
git add .codex-plugin/plugin.json launchers/runtime-id
```

Включи оба файла в PR. CI проверит версию и соответствие идентификатора
исходникам. После обновления базы повтори подготовку. Для preview без публикации
используй `bin/fstack --package . release plan`.

Перед слиянием требуются успешные проверки текущей ревизии. Сохрани
проверенный заголовок при squash или используй обычный merge. Для настройки
защиты веток применяй [декларацию ruleset](../.github/release-branches.ruleset.json)
с учётом действующих правил проекта. Обязательные jobs бери из текущей матрицы
и включи проверку актуальной базы PR.

После слияния workflow сверяет SHA с удалённой `main`, проверяет историю версий,
собирает выпуск из проверенных артефактов и публикует ZIP, бинарники и SHA-256.
При временном сбое повтор продолжается из черновика с подтверждёнными
источником и артефактами. Несовпадение SHA, версии или контрольной суммы
требует исправления до публикации. Опубликованные выпуски сохраняются.
