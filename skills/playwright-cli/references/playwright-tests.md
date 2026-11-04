# Запуск и диагностика тестов Playwright

Используй runner и команды проекта. Чтобы HTML-отчёт не открывал интерактивный
браузер, установи `PLAYWRIGHT_HTML_OPEN=never`.

Bash:

```bash
PLAYWRIGHT_HTML_OPEN=never npx --no-install playwright test tests/example.spec.ts
```

PowerShell:

```powershell
$env:PLAYWRIGHT_HTML_OPEN = 'never'
npx --no-install playwright test tests/example.spec.ts
```

Для поддерживаемой установленной версией диагностики используй `--debug=cli`.
Запусти нужный тест фоновым процессом через доступный инструмент оболочки,
прочитай вывод до `Debugging Instructions` и имени сессии `tw-…`.

```bash
npx --no-install playwright test tests/example.spec.ts --debug=cli
playwright-cli attach tw-abcdef
playwright-cli snapshot
```

Тест приостановлен в начале. Используй команды stepping из фактически выданной
справки, чтобы дойти до сбоя. Сохраняй пользовательскую подготовку и fixtures:
обычное `open` по URL может пропустить их. CLI выводит эквивалент действий
на TypeScript; он помогает исследованию, но не доказывает правильность ожидания.

Различай дефект приложения, фикстуры, данных и locator. Право диагностировать
не разрешает менять тест или его контракт. Если исправление входит в поручение,
повтори исходный сценарий после него по verify-result. При завершении отсоедини
свою CLI-сессию и останови только принадлежащий тебе фоновый тестовый процесс.
Не закрывай пользовательский браузер.
