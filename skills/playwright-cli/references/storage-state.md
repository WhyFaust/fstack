# Состояние и хранилища браузера

Команды действуют в выбранной сессии и текущем origin. Сохраняемое состояние
может содержать авторизацию. Используй только данные и учётную запись,
разрешённые поручением; не выводи секреты в чат, логи или Git.

## Сохранение и восстановление

```bash
playwright-cli -s=task state-save session.auth-state.json
playwright-cli -s=task state-load session.auth-state.json
playwright-cli -s=task goto https://example.com
```

`state-save` без имени создаёт файл с временной меткой. Формат содержит cookies
и origins с localStorage; дополнительные части зависят от поддерживаемой версии.
Не обещай перенос всей browser session: sessionStorage, вкладки и произвольное
состояние приложения не следует считать автоматически сохранёнными.
После восстановления проверь фактическую авторизацию и origin.

## Cookies

```bash
playwright-cli cookie-list --domain=example.com
playwright-cli cookie-list --path=/api
playwright-cli cookie-get session_id
playwright-cli cookie-set session_id demo --domain=example.com --path=/ --httpOnly --secure --sameSite=Lax
playwright-cli cookie-set remember_me demo --expires=1893456000
playwright-cli cookie-delete session_id
```

`cookie-list` без фильтра показывает все доступные cookies.
`cookie-clear` удаляет их в контексте: используй только в собственном
тестовом контексте. Для нескольких значений доступен
`page.context().addCookies([...])` через [running-code.md](running-code.md).

## localStorage и sessionStorage

```bash
playwright-cli localstorage-list
playwright-cli localstorage-get theme
playwright-cli localstorage-set theme dark
playwright-cli localstorage-set preferences '{"theme":"dark","language":"ru"}'
playwright-cli localstorage-delete theme
playwright-cli sessionstorage-list
playwright-cli sessionstorage-get step
playwright-cli sessionstorage-set step 3
playwright-cli sessionstorage-delete step
```

`localstorage-clear` и `sessionstorage-clear` очищают соответствующее хранилище
текущего origin. Применяй их только для данных текущей работы. Для нескольких
операций используй `page.evaluate` с передачей значений аргументом.

## IndexedDB

Список баз можно прочитать через `page.evaluate(() => indexedDB.databases())`.
Удаление базы является отдельным изменением данных и не нужно для обычной
диагностики. При разрешённой очистке дождись завершения IDBRequest и обработай
ошибку либо блокировку; вызов `deleteDatabase` сам по себе не доказывает удаление.

## Владение файлами

Храни auth state вне отслеживаемых исходников или исключи его точным правилом
`*.auth-state.json`. Сохраняй только на необходимый срок и удаляй лишь файлы,
созданные этой работой. Не стирай пользовательские постоянные профили.
Повторно используй авторизацию только в пределах разрешённого сценария.
