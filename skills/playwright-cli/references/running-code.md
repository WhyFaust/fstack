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

```javascript
async page => {
  await page.context().grantPermissions(['geolocation'], {
    origin: 'https://example.com',
  });
  await page.context().setGeolocation({
    latitude: 51.5074,
    longitude: -0.1278,
  });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  return page.viewportSize();
}
```

Выдавай только необходимые сценарию browser permissions в собственной сессии.
`clearPermissions` снимает разрешения, но сам по себе не доказывает сброс
географического override. Временную среду надёжнее завершить вместе со своим
контекстом. Для media можно задавать `media: 'print'`.

## Ожидание наблюдаемого результата

```javascript
async page => {
  await page.locator('.loading').waitFor({ state: 'hidden' });
  await page.locator('.result').waitFor({ timeout: 10000 });
  await page.waitForFunction(() => window.appReady === true);
  return page.url();
}
```

Выбирай условие по контракту. Отсутствие сети не означает готовность интерфейса;
произвольная пауза не устраняет гонку. Таймаут ограничивает ожидание,
а не заменяет нужный сигнал.

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
