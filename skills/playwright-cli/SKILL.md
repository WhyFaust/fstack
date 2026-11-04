---
name: playwright-cli
allowed-tools: Bash(playwright-cli:*) Bash(npx playwright:*) Bash(npx --no-install playwright:*)
description: Управляет веб-страницами через Playwright CLI и помогает создавать или диагностировать Playwright-тесты.
  Используй для браузерных взаимодействий, snapshot, UI-воспроизведения, trace и скриншотов; нативные
  Windows-приложения требуют computer-use из текущего каталога.
metadata:
  short-description: Работа с веб-интерфейсом через Playwright CLI
  version: 0.1.3
---

# Браузер через Playwright


Выбери именованную сессию, если браузером пользуется другая работа. Перед действием
получи свежий snapshot и используй ссылки на элементы из него. После навигации или
существенного изменения DOM обнови ссылки. Сохраняй подключённый пользователем
браузер; закрывай только собственные ресурсы.

```bash
playwright-cli -s=fstack-task open "https://example.com"
playwright-cli -s=fstack-task snapshot
playwright-cli -s=fstack-task click e3
playwright-cli -s=fstack-task snapshot
playwright-cli -s=fstack-task screenshot --filename=work/page.png
playwright-cli -s=fstack-task close
```

В PowerShell URL с `&` передавай в кавычках. Артефакты сохраняй в рабочую область
и просматривай перед выводом об интерфейсе. Версию и неизвестные флаги проверяй
через `playwright-cli --version` и `playwright-cli --help <command>`.
Разрешение открыть страницу само по себе не разрешает отправку сообщения,
покупку или изменение данных.

## Справки по задаче

| Задача | Справка |
| --- | --- |
| Команды, вкладки, эмуляция | [commands.md](references/commands.md) |
| Владение сессией | [session-management.md](references/session-management.md) |
| Cookies и локальное состояние | [storage-state.md](references/storage-state.md) |
| Нестандартный Playwright-код | [running-code.md](references/running-code.md) |
| Атрибуты и локаторы | [element-attributes.md](references/element-attributes.md) |
| Запуск и диагностика тестов | [playwright-tests.md](references/playwright-tests.md) |
| Проектирование и исправление тестов | [test-generation.md](references/test-generation.md) |
| Подмена запросов | [request-mocking.md](references/request-mocking.md) |
| Трассировка | [tracing.md](references/tracing.md) |
| Запись видео | [video-recording.md](references/video-recording.md) |
| Вложения в PR | [pr-attachments.md](references/pr-attachments.md) |

Читай только справку нужного режима. Установка CLI и браузера — отдельное изменение
среды; используй существующий инструмент, если его достаточно.
Источник: [Microsoft Playwright CLI](https://github.com/microsoft/playwright-cli).
