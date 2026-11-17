---
name: playwright-cli
description: 'Выполняет веб-сценарии и диагностирует Playwright-тесты через Playwright CLI, используя snapshot, снимки экрана и трассировку. Визуальное проектирование относится к ui, работа с нативными приложениями требует другого инструмента.'
disable-model-invocation: true
---

# Браузер через Playwright

## Вход и границы

При параллельной работе выдели сессию, профиль и данные. Используй ссылку из
свежего snapshot той же вкладки. После навигации, смены вкладки или DOM обнови
снимок. Действия с внешним эффектом выполняй в рамках полномочий.

## Порядок работы

Проверь `playwright-cli --version` и флаги через `playwright-cli --help <command>`.
Если CLI отсутствует, проверь `playwright cli --help` и подставь `playwright cli`
в команды. Локальный npm-пакет запускай через runner проекта без установки.
Если оба входа недоступны, укажи проверенный шаг. В PowerShell заключай URL с
`&` в кавычки.

Подставь имя сессии, URL и ссылку из snapshot в команды Bash.

```bash
playwright-cli -s="$session" open "$url"
playwright-cli -s="$session" snapshot
playwright-cli -s="$session" click "$element_ref"
playwright-cli -s="$session" snapshot
playwright-cli -s="$session" close
```

Закрывай только свой `open`. Для `attach` используй `detach`. Перед выдачей
разрешений проверь origin. Защищай cookies и storage state.

В тесте выбирай локатор по роли, подписи или test ID, ожидай требуемого состояния.
Сверяй выборку через `playwright test --list` с тем же runner и фильтром.
Укажи, какую часть интеграции исключает подмена ответа.

[Playwright CLI](https://github.com/microsoft/playwright-cli),
[Playwright Test](https://playwright.dev/docs/test-cli).
Материалы Playwright CLI адаптированы. Copyright (c) Microsoft Corporation.
[Apache-2.0](../../licenses/Apache-2.0.txt). Собственные инструкции fstack распространяются по MIT.

## Проверка и результат

Подтверди эффект свежим snapshot, состоянием или сохранённым результатом.
При сбое укажи шаг и освободи свою сессию. Сохрани артефакты в рабочей области
и проверь содержание и полномочия перед передачей.
