---
name: playwright-cli
description: "Проверяет веб-сценарии и разбирает сбои тестов через Playwright CLI."
disable-model-invocation: true
metadata:
  version: "2.0.6"
---

# Браузер через Playwright

[Каталог · Проверка](../../docs/skills.md#проверка)

Цитата не включает навык. Вне faust-mode применяй только явно вызванные навыки.

Сверь `playwright-cli --version` и `playwright-cli --help <command>`. Запасной вход — `playwright cli --help`. Локальный npm-пакет запускай через runner проекта без установки. При отказе укажи проверенный шаг. В PowerShell URL с `&` заключай в кавычки.

Параллельным прогонам выдели отдельные сессии, профили и данные. Бери ссылку из snapshot текущей вкладки. Обнови снимок после навигации, смены вкладки или DOM.

```bash
playwright-cli -s="$session" open "$url"
playwright-cli -s="$session" snapshot
playwright-cli -s="$session" click "$element_ref"
playwright-cli -s="$session" snapshot
playwright-cli -s="$session" close
```

Проверь полномочия на действия и origin перед выдачей прав. Защити cookies и storage state. Подтверди эффект снимком или состоянием. При сбое укажи шаг. Закрывай свой `open`, для `attach` используй `detach`. Артефакты оставь в рабочей области, перед передачей сверь содержимое и права.

В тесте жди нужного состояния по роли, подписи или test ID. Проверь выборку через `playwright test --list` с тем же runner и фильтром. Укажи, что исключила подмена ответа.

[Playwright CLI](https://github.com/microsoft/playwright-cli),
[Playwright Test](https://playwright.dev/docs/test-cli).
Материалы Playwright CLI адаптированы. Copyright (c) Microsoft Corporation.
[Apache-2.0](../../licenses/Apache-2.0.txt). Собственные инструкции fstack распространяются по MIT.
