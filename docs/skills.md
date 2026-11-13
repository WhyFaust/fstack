# Каталог навыков

По умолчанию методы fstack не выбираются автоматически. Полный процесс
включается через `/faust-mode`; отдельный метод можно вызвать явно.

Один навык отвечает за один понятный результат. Выбирай основной метод по
текущей задаче, затем добавляй только нужный предметный приём. Полный каталог
не загружается как обязательная последовательность.
Все навыки следуют [единому шаблону](../skills/adapt-skill/references/template.md):
вход и границы, порядок работы, проверка и результат. Предметные шаги различаются;
при создании и доработке форму поддерживает `adapt-skill`.

## Короткий playbook

Выбирай основной навык по ожидаемому результату. Если подходят несколько,
сопоставь их условия через [select-skills](../skills/select-skills/SKILL.md).
Глубину проверки определи по [verify-result](../skills/verify-result/SKILL.md#выбрать-глубину-по-риску).

## Основные методы

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `select-skills` | Один основной метод и необходимые предметные дополнения | [Открыть](../skills/select-skills/SKILL.md) |
| `investigate-code` | Исследование кода по исходным данным | [Открыть](../skills/investigate-code/SKILL.md) |
| `improve-codebase` | Аудит и приоритетный backlog улучшений | [Открыть](../skills/improve-codebase/SKILL.md) |
| `plan-improvement` | План выбранного улучшения и сверка его актуальности | [Открыть](../skills/plan-improvement/SKILL.md) |
| `implement-change` | Реализация законченного изменения | [Открыть](../skills/implement-change/SKILL.md) |
| `adapt-skill` | Создание, перенос и улучшение исходников навыков | [Открыть](../skills/adapt-skill/SKILL.md) |
| `debug-failure` | Причина сбоя и проверка исправления | [Открыть](../skills/debug-failure/SKILL.md) |
| `refactor-code` | Упрощение кода с сохранением контрактов | [Открыть](../skills/refactor-code/SKILL.md) |
| `review-change` | Ревью изменения по контрактам и фактам | [Открыть](../skills/review-change/SKILL.md) |
| `verify-result` | Проверки и доказательства готовности | [Открыть](../skills/verify-result/SKILL.md) |

## Инженерные решения

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `engineering-principles` | Критерии архитектуры, состояния и проверки | [Открыть](../skills/engineering-principles/SKILL.md) |
| `design-change` | Проектирование модели и границ изменения | [Открыть](../skills/design-change/SKILL.md) |
| `design-ui` | Пользовательский сценарий, компоненты и визуальная проверка UI | [Открыть](../skills/design-ui/SKILL.md) |
| `assess-impact` | Последствия изменения и затронутые потребители | [Открыть](../skills/assess-impact/SKILL.md) |
| `simplify-change` | Упрощение готового diff без расширения задачи | [Открыть](../skills/simplify-change/SKILL.md) |
| `manage-state` | Владение состоянием и конкурентными записями | [Открыть](../skills/manage-state/SKILL.md) |
| `model-check-state` | Проверка свойства автомата по коду и трасса в Mermaid | [Открыть](../skills/model-check-state/SKILL.md) |
| `manage-resources` | Жизненный цикл и очистка собственных ресурсов | [Открыть](../skills/manage-resources/SKILL.md) |
| `migrate-contract` | Миграция контрактов и всех потребителей | [Открыть](../skills/migrate-contract/SKILL.md) |
| `measure-performance` | Измерение ускорения без потери покрытия | [Открыть](../skills/measure-performance/SKILL.md) |
| `trace-history` | Причины решений по истории и источникам | [Открыть](../skills/trace-history/SKILL.md) |

## Выполнение и продолжение

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `faust-mode` | Включение полного процесса на весь чат до `/faust-mode-off` | [Открыть](../skills/faust-mode/SKILL.md) |
| `plan-work` | План законченных этапов, зависимостей и проверок | [Открыть](../skills/plan-work/SKILL.md) |
| `delegate-work` | Поручения, владение и завершение подагентов | [Открыть](../skills/delegate-work/SKILL.md) |
| `checkpoint-task` | Сохранение проверяемой точки продолжения | [Открыть](../skills/checkpoint-task/SKILL.md) |
| `resume-task` | Продолжение задачи со сверкой живого состояния | [Открыть](../skills/resume-task/SKILL.md) |
| `prepare-verification` | Подготовка воспроизводимой проверки проекта | [Открыть](../skills/prepare-verification/SKILL.md) |
| `verify-project` | Проверка проекта через реальный сценарий | [Открыть](../skills/verify-project/SKILL.md) |
| `maintain-verification` | Поддержание актуальности проектной проверки | [Открыть](../skills/maintain-verification/SKILL.md) |

## PR и MR

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `watch-pr` | Состояние и сопровождение PR или MR | [Открыть](../skills/watch-pr/SKILL.md) |
| `resolve-review` | Разбор и исправление замечаний ревью | [Открыть](../skills/resolve-review/SKILL.md) |
| `ship-pr` | Слияние проверенной ревизии PR или MR | [Открыть](../skills/ship-pr/SKILL.md) |

## Предметные инструменты

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `python` | Python-код, окружение, типы и ресурсы; тематические справки по задаче | [Открыть](../skills/python/SKILL.md) |
| `no-comments` | Понятный код без поясняющих комментариев и закомментированных фрагментов | [Открыть](../skills/no-comments/SKILL.md) |
| `find-docs` | Проверка API по документации нужной версии | [Открыть](../skills/find-docs/SKILL.md) |
| `glab` | GitLab CLI для операций, недоступных через MCP | [Открыть](../skills/glab/SKILL.md) |
| `playwright-cli` | Работа с веб-интерфейсом через Playwright CLI | [Открыть](../skills/playwright-cli/SKILL.md) |

## Работа с текстом

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `write-text` | Новый авторский текст, комментарий, ответ, идеи, названия, план или адаптация под другую аудиторию, формат или площадку. Вызов `$fstack:write-text` | [Открыть](../skills/write-text/SKILL.md), [English](../skills/write-text/SKILL_en.md) |
| `edit-text` | Вычитка, упрощение или сокращение текста для той же аудитории в прежнем формате. Вызов `$fstack:edit-text` | [Открыть](../skills/edit-text/SKILL.md), [English](../skills/edit-text/SKILL_en.md) |
| `research-for-text` | Подбор материалов, поиск свежих поводов и проверка фактов с источниками. Вызов `$fstack:research-for-text` | [Открыть](../skills/research-for-text/SKILL.md), [English](../skills/research-for-text/SKILL_en.md) |
| `deepread` | Разбор присланного материала и выполнение поручения по нему с проверкой данных по источнику. Вызов `$fstack:deepread` | [Открыть](../skills/deepread/SKILL.md) |

Поиск можно запросить отдельно или подключить к написанию и правке, когда нужны
новые материалы или проверка фактов.

## Память и внешние компоненты

| Навык | Когда применять | Инструкция |
| --- | --- | --- |
| `use-memory` | Поправки проекта и общие предпочтения в Obsidian | [Открыть](../skills/use-memory/SKILL.md) |
| `use-rtk` | Сокращение шума RTK с сохранением доказательств | [Открыть](../skills/use-rtk/SKILL.md) |
| `codex-config-evaluation` | Контролируемое сравнение настроек и навыков | [Открыть](../skills/codex-config-evaluation/SKILL.md) |

## Правила выбора

При включённом faust-mode координатор передаёт исполнителю цель и границы
поручения. Исполнитель открывает нужный метод и только те справки, которые
влияют на решение. Если меняется ожидаемый результат, он заново выбирает
основной навык. [Порядок делегирования](orchestration.md#от-поручения-к-приёмке).

Для отдельного метода используй полное имя `$fstack:<имя-навыка>` из
каталога клиента. При совпадении имён проверь источник установленного навыка.
Сравнение моделей и конфигураций через `codex-config-evaluation` требует
отдельного поручения.
