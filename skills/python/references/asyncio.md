# Отказы конкурентных операций в asyncio

Определи, что получит вызывающий код при сбое одной операции, кто дождётся остальных и кто освободит ресурсы. Сначала проверь минимальную версию Python и зависимость между шагами. Если следующему шагу нужен результат предыдущего, дождись его отдельно.

- Если общий результат теряет смысл при ошибке одного участника, используй `asyncio.TaskGroup` с Python 3.11. При первом исключении, кроме `CancelledError`, группа отменит незавершённых участников, дождётся их и передаст ошибки в группе исключений. `KeyboardInterrupt` и `SystemExit` после очистки передаются напрямую. При отмене вызывающей задачи дай участникам завершить очистку в `finally` или `async with` и передай отмену дальше.
- Если первый сбой нужно вернуть сразу, а остальные операции должны продолжиться, подходит обычный `gather`. При общем успехе он возвращает результаты в порядке входа. При первом исключении он сразу передаёт его вызывающему коду, оставляя прочие задачи работать. Сохрани ссылки на задачи и передай их владельцу, который дождётся завершения и обработает исходы. На Python до 3.11 для отмены всей операции при сбое явно отмени незавершённые задачи, дождись их очистки и сохрани первичную ошибку. Вызов `gather.cancel()` после передачи первого исключения оставит прочие задачи работать.
- Если нужны все независимые исходы, дождись их и свяжи каждый с его входом. Преобразуй ожидаемую ошибку в явный статус. При `gather(..., return_exceptions=True)` разбери каждый исход и передай неожиданные ошибки и отмену дальше. Отмена самого `gather` передаётся незавершённым задачам независимо от этого флага.

Например, сводка требует сведений о рейсе и его маршруте. При недоступности маршрута чтение рейса больше не нужно. Для независимой диагностики, наоборот, важен ответ каждого источника. Пример для Python 3.11 и новее запускается через pytest без плагина asyncio.

```python
import asyncio

import pytest


class SourceUnavailable(Exception):
    pass


async def load_summary(read_trip, read_route):
    async with asyncio.TaskGroup() as group:
        trip = group.create_task(read_trip())
        route = group.create_task(read_route())
    return trip.result(), route.result()


async def inspect_sources(**readers):
    results = await asyncio.gather(
        *(read() for read in readers.values()), return_exceptions=True
    )
    statuses = {}
    for source, result in zip(readers, results):
        if isinstance(result, SourceUnavailable):
            statuses[source] = "unavailable"
        elif isinstance(result, BaseException):
            raise result
        else:
            statuses[source] = result
    return statuses


def test_summary_waits_for_cleanup_on_failure():
    async def scenario():
        opened = asyncio.Event()
        released = asyncio.Event()

        async def read_trip():
            opened.set()
            try:
                await asyncio.Event().wait()
            finally:
                await asyncio.sleep(0)
                released.set()

        async def read_route():
            await opened.wait()
            raise SourceUnavailable("route offline")

        with pytest.raises(ExceptionGroup) as failure:
            await load_summary(read_trip, read_route)
        assert tuple(type(error) for error in failure.value.exceptions) == (SourceUnavailable,)
        assert released.is_set()

    asyncio.run(scenario())


def test_diagnostics_returns_both_outcomes():
    async def read_trip():
        return "reachable"

    async def read_route():
        raise SourceUnavailable("route offline")

    assert asyncio.run(inspect_sources(route=read_route, trip=read_trip)) == {
        "trip": "reachable",
        "route": "unavailable",
    }
```

Введи управляемый сбой после получения ресурса и проверь очистку до выхода из операции. Отдельно отмени саму операцию и проверь прохождение `CancelledError`. Для независимых исходов проверь частичный успех и выход неожиданной ошибки наружу. Синхронизируй задачи через события. Завершение `asyncio.run()` само отменяет оставшиеся задачи, поэтому проверяй очистку внутри ещё работающего цикла.

Запись, уже зафиксированная внешней системой, сохраняется после отмены задачи. Для атомарности используй транзакцию по контракту системы. Если контракт предусматривает компенсацию, обработай частичную запись по его правилам. При потере ответа сверь состояние по идентификатору операции до повтора.
