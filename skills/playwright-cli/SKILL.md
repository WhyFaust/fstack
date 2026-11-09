---
name: playwright-cli
description: 'Выполняет веб-сценарии и диагностирует Playwright-тесты через Playwright CLI, используя snapshot, снимки экрана и трассировку. Визуальное проектирование относится к design-ui, работа с нативными приложениями требует другого инструмента.'
license: MIT AND Apache-2.0
compatibility: Установленный Playwright CLI и доступный ему браузер; Node.js нужен при запуске через npx.
metadata:
  short-description: Работа с веб-интерфейсом через Playwright CLI
  version: 1.0.2
  author: WhyFaust
  source: https://github.com/WhyFaust/fstack
---

# Браузер через Playwright

Выбери именованную сессию, если браузером пользуется другая работа. Перед действием
получи свежий snapshot и используй ссылки на элементы из него. После навигации или
существенного изменения DOM обнови ссылки. При смене вкладки получи snapshot именно
её содержимого, прежде чем использовать ссылку на элемент. Если открыта существующая
пользовательская сессия, проверь нужную вкладку и способ завершения по
[справке о сессиях](references/session-management.md). Закрывай только
собственные ресурсы.

```bash
playwright-cli -s=fstack-task open "https://example.com"
playwright-cli -s=fstack-task snapshot
```

Выбери ссылку нужного элемента из этого snapshot. Например, если это `e3`,
выполни следующие команды.

```bash
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
Справки адаптированы из [Microsoft Playwright CLI](https://github.com/microsoft/playwright-cli)
по Apache-2.0; собственные инструкции fstack распространяются по MIT.
При распространении сохраняй [уведомление](../../NOTICE.md)
и [текст Apache-2.0](../../licenses/Apache-2.0.txt).
