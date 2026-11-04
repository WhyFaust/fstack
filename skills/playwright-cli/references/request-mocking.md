# Перехват сетевых запросов

Подмена подходит для управляемой проверки UI-ветки, ошибки или граничного
ответа. Она не подтверждает реальную интеграцию, которую заменяет.
Включай маршруты до действия, запускающего запрос, и снимай свои маршруты после.

```bash
playwright-cli route "**/*.jpg" --status=404
playwright-cli route "**/api/users" --body='[{"id":1,"name":"Alice"}]' --content-type=application/json
playwright-cli route "**/api/data" --body='{"ok":true}' --header="X-Custom: value"
playwright-cli route "**/*" --remove-header=cookie,authorization
playwright-cli route-list
playwright-cli unroute "**/*.jpg"
```

Glob-шаблоны: `**/api/users` — путь, `**/api/*/details` — сегмент,
`**/*.{png,jpg,jpeg}` — расширения, `**/search?q=*` — параметры.
`unroute` без шаблона снимает все маршруты сессии: применяй только в собственной.

Для условного ответа, изменения настоящего ответа и моделирования отказа
используй `run-code --filename=route.js`. Файл содержит одну функцию-выражение.

```javascript
async page => {
  await page.route('**/api/login', async route => {
    const body = route.request().postDataJSON();
    await route.fulfill({
      status: body.username === 'admin' ? 200 : 401,
      json: body.username === 'admin'
        ? { token: 'mock-token' }
        : { error: 'Invalid' },
    });
  });
}
```

Изменение настоящего ответа:

```javascript
async page => {
  await page.route('**/api/user', async route => {
    const response = await route.fetch();
    const json = await response.json();
    await route.fulfill({ response, json: { ...json, isPremium: true } });
  });
}
```

Для отказа: `route.abort('internetdisconnected')`; другие подходящие коды —
`connectionrefused`, `timedout`, `connectionreset`. Для искусственной задержки
в обработчике допустим `await new Promise(resolve => setTimeout(resolve, 3000))`,
после которого явно завершается `route.fulfill`. Это модель медленного ответа,
а не замена ожидания готовности в тесте.

Выбирай только нужный перехват и не сохраняй auth payload в публикуемых данных.
