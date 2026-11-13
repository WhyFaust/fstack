# Пользовательский код Playwright

Используй `run-code`, когда обычных команд CLI недостаточно. Для многострочного
кода файл удобнее и безопаснее shell-интерполяции.

```bash
playwright-cli run-code --filename=script.js
playwright-cli run-code "async page => await page.title()"
```

Файл содержит одно выражение функции `async page => { … }`. Это изолированный
контекст, а не Node.js: `import`, `export`, `require`, `process` и Node-модули
недоступны. Поддерживаются timers, fetch, URL, Buffer, crypto, AbortController,
TextEncoder и TextDecoder. Внутри `page.evaluate` выполняется код страницы;
данные передавай аргументом, а не конструированием исполняемой строки.

## Состояние, разрешения и отображение

Перед выдачей разрешения сравни origin открытой страницы с заранее заданным
HTTP(S) origin проверяемого приложения. Если страница ещё не открыта или
после перенаправления origin изменился, остановись. Вызови
`page.context().grantPermissions(permissions, { origin: expectedOrigin })`
в собственном контексте. Для координат используй
`page.context().setGeolocation({ latitude, longitude })`, для режима
отображения — `page.emulateMedia(options)`.

`clearPermissions` сбрасывает разрешения. Чтобы также
убрать переопределённое местоположение, заверши временный контекст.
Для media можно задавать `media: 'print'`.

## Ожидание наблюдаемого результата

```javascript
async page => {
  const result = page.getByRole('status');
  await result.waitFor({ state: 'visible', timeout: 10000 });
  return result.textContent();
}
```

Выбирай locator и условие по контракту. Если готовность отражена в состоянии
страницы, проверяй его через `page.waitForFunction`. Ограничь ожидание таймаутом.
Сетевая тишина и фиксированная пауза сами по себе не подтверждают готовность.

## Frames, загрузки и данные

```javascript
async page => {
  const frame = page.locator('iframe#my-iframe').contentFrame();
  await frame.getByRole('button', { name: 'Continue' }).click();
  return page.frames().map(frame => frame.url());
}
```

Ожидание download регистрируй до действия:

```javascript
async page => {
  const pending = page.waitForEvent('download');
  await page.getByRole('link', { name: 'Download' }).click();
  const download = await pending;
  await download.saveAs('./downloaded-file.pdf');
  return download.suggestedFilename();
}
```

Сохраняй файл в разрешённой области и проверь его перед передачей.
Для небольшого извлечения используй `locator.allTextContents()`, `page.title()`
или `page.evaluate`. Не выгружай весь HTML без необходимости.

Clipboard — часть пользовательского состояния. Читай или меняй его только
в рамках запроса, учитывая browser permissions. Авторизация и сохранённый
storage state могут содержать секреты; не выводи их в чат или Git.

## Ошибки и внешние действия

Не заменяй неизвестную ошибку успешной строкой в широком catch. Сохраняй причину
и отдельно обрабатывай только ожидаемый отказ по контракту.
Разрешение выполнить код браузера не разрешает отправку формы, покупку,
публикацию или изменение несвязанных данных.
