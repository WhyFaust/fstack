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

В Linux CI работают Python-проверки измерителя и runner без моделей.

```bash
python -m unittest discover -s tools/fstack/benchmarks -p 'test_*.py' -v
python -m unittest discover -s tools/fstack/evals -p 'test_*.py' -v
```

## Проверка фактическим клиентом

```bash
bin/fstack check-installation --codex /absolute/path/to/codex
```

Команда устанавливает пакет в отдельный временный профиль. Она сверяет
версию и содержимое навыков, обнаружение хуков и загрузку конфигурации.
В отчёте указывай версию клиента. Выполнение задач моделью проверяется отдельно.

## Изменение навыка

Следуй [skill](../skills/skill/SKILL.md). Обнови
[каталог](skills.md) при добавлении, переименовании или удалении навыка.
Повышай `metadata.version` только у содержательно изменённых навыков,
согласованно во всех локализациях. Новый навык начинает с `1.0.0`.

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
снимок с базой PR и проверяет версии пакета и изменённых навыков.

После слияния проверь выпуск из `main`. Workflow сверяет SHA, использует
артефакты проверенных сборок и публикует архивы, бинарники и SHA-256.
Предпросмотр `release plan` проверяет выбранную в манифесте версию.
При временном сбое повтор продолжает принадлежащий выпуску черновик
с той же проверенной версией и исходниками.
