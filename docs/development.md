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

Список платформ и флаги сборки задаёт [CI](../.github/workflows/release.yml).
Проверяй память во временных vault и профиле вне Git. `check-package` проверяет
ресурсы пакета, локальные ссылки и якоря.

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

Проверки используют отдельный профиль. Первая сверяет установленную версию,
навыки и хуки. Вторая проверяет `doctor` до установки, после установки и после
одобрения хуков. Третья читает запросы клиента к локальной заглушке:

- обычный чат и обсуждение названий сохраняют выключенный режим;
- прямой вызов передаёт выбранную инструкцию; в цитате клиент тоже может
  приложить файл, поэтому проверяется правило о цитировании;
- включение передаёт имена и условия выбора всех установленных навыков.
  Продолжение обходится без повторного индекса. Выключение и соседний чат
  проверяются отдельно.

CI запускает эти проверки на двух версиях Codex CLI. Чтобы проверить действия
модели на GitLab MR, дай ей снимок на чтение и сверь по журналу, что чтение
`watch-pr/SKILL.md` предшествовало обращению к MR. Модельные прогоны требуют поручения.

В Desktop проверь обычный ход без записи (`phase: skipped`) и поправку из поля
ввода (`memory stage`, затем запись с правильным `session_id`). Для поручения Codex
и подагента проверь поиск, `memory report` на первом `Stop` или `SubagentStop`
и сохранение на следующем. Прерванный ход и Plan пропускают запись. При смене
формата журнала повтори сценарии. [Контракт хуков](https://learn.chatgpt.com/docs/hooks#common-input-fields).

## Изменение навыка

Сверь [формат](../skills/skill/SKILL.md), [каталог](skills.md) и локализации.
Сохрани лицензию и атрибуцию.

## Версии и публикация

Заполни [шаблон PR](../.github/PULL_REQUEST_TEMPLATE.md). Исправление повышает
PATCH, возможность или несовместимость в серии 0.x — MINOR. Для MAJOR требуется
прямое указание и изменение [package-major](../launchers/package-major).

Добавь входы сборки в индекс Git, затем выполни:

```bash
bin/fstack --package . release prepare VERSION
git add .codex-plugin/plugin.json launchers/package-version launchers/runtime-id
```

Без аргумента `release prepare` пересчитывает идентификатор для выбранной версии.
Новая версия нужна для изменений навыков, каталога, манифеста, интерфейса,
marketplace, лицензий и входов сборки. CI сверяет базу PR, имя выпуска в GitHub
Releases и коммит очереди слияния.

Если пользователю нужны действия при обновлении, добавь в PR заголовок `## При обновлении`. Издатель переносит его в GitHub Release.
План публикации показывает `release plan`. После слияния проверь SHA, архивы,
бинарники и SHA-256 выпуска из `main`. При временном сбое повторный запуск
продолжает черновик той же сборки.
