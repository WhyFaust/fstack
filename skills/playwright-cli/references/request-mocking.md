# Перехват сетевых запросов

Подмена подходит для управляемой проверки UI-ветки, ошибки или граничного
ответа. Она не подтверждает реальную интеграцию, которую заменяет.
Включай маршруты до действия, запускающего запрос, и снимай свои маршруты после.

Передай в `pattern` glob-шаблон целевых URL, в `response_json` — тело ответа.
Для статуса используй `--status`, для заголовков — `--header` или `--remove-header`.

```bash
playwright-cli route "$pattern" --body="$response_json" --content-type=application/json
playwright-cli route-list
playwright-cli unroute "$pattern"
```

В glob-шаблоне `*` соответствует части одного сегмента пути, `**` — нескольким
сегментам. `unroute` без шаблона снимает все маршруты сессии.

Для условного или изменённого ответа используй `run-code --filename=route.js`.
Зарегистрируй обработчик через `await page.route(pattern, async route => { … })`.
В обработчике выбери ветку по `route.request()` и верни ожидаемые статус и
данные через `route.fulfill({ status, json })`. Если нужен исходный ответ,
получи его через `route.fetch()`, затем передай в `fulfill` вместе с изменениями.

Для отказа: `route.abort('internetdisconnected')`; другие подходящие коды —
`connectionrefused`, `timedout`, `connectionreset`. Для искусственной задержки
в обработчике допустим `await new Promise(resolve => setTimeout(resolve, 3000))`,
после которого явно завершается `route.fulfill`. Это модель медленного ответа,
а не замена ожидания готовности в тесте.

Выбирай только нужный перехват и не сохраняй auth payload в публикуемых данных.
