# Запись видео

Видео WebM показывает пользовательский сценарий; оно дополняет проверки,
но не заменяет assertions, сетевые доказательства и trace.

## Короткая запись

```bash
playwright-cli -s=demo open https://example.com
playwright-cli -s=demo video-start demo.webm --cursor --fps=60
playwright-cli -s=demo video-chapter "Начало" --description="Открытие формы" --duration=2000
playwright-cli -s=demo snapshot
playwright-cli -s=demo click e1
playwright-cli -s=demo video-stop
playwright-cli -s=demo close
```

Путь сохранения проверь по выводу и просмотри фактический файл.
`--cursor` добавляет анимированный указатель и замедляет показ действий,
чтобы его движение было видно. Обозначение главы — часть демонстрации,
а не ожидание готовности продукта.

## Подписи и выделение

```bash
playwright-cli video-show-actions --duration=800 --position=top-right --highlight-style="outline: 2px solid #333" --point-style="width: 20px; height: 20px; border-radius: 50%; background: rgba(255,0,0,.7)" --title-style="font-size: 16px"
playwright-cli video-hide-actions
```

Помимо cursor можно показывать точку клика, область элемента и название действия.
Стили — CSS declarations. `outline` не меняет размер выделенной области.
Подсветка и указатель не доказывают успешность самого действия.

## Сценарий из файла

Когда нужен управляемый показ, сначала исследуй интерфейс и устойчивые locators,
затем запиши одну функцию для `run-code --filename=demo.js`.

```javascript
async page => {
  await page.screencast.start({
    path: 'demo.webm',
    size: { width: 1280, height: 800 },
    fps: 60,
  });
  let scenarioError;
  try {
    await page.screencast.showActions({
      cursor: 'pointer',
      duration: 800,
      position: 'top-right',
      style: {
        highlight: 'outline: 2px solid #333',
        title: 'font-size: 16px',
      },
    });
    await page.goto('https://example.com');
    await page.screencast.showChapter('Проверка формы', {
      description: 'Выполнение разрешённого сценария',
      duration: 2000,
    });
    await page.getByRole('button', { name: 'Continue' }).click();
  } catch (error) {
    scenarioError = error;
    throw error;
  } finally {
    try {
      await page.screencast.stop();
    } catch (cleanupError) {
      if (scenarioError) {
        throw new AggregateError([scenarioError, cleanupError], 'Сценарий и завершение записи не удались');
      }
      throw cleanupError;
    }
  }
}
```

Этот пример не запускается автоматически. Если запись или stop завершились
ошибкой, сохрани обе причины, не подменяя исходный сбой успехом очистки.

## API оформления

| Вызов | Назначение |
| --- | --- |
| `showChapter(title, { description?, duration?, styleSheet? })` | Полноэкранная карточка главы |
| `showOverlay(html, { duration? })` | HTML-подпись или выделение |
| `disposable.dispose()` | Удаление постоянного overlay |
| `hideOverlays() / showOverlays()` | Временно скрыть или показать overlays |
| `showActions({ cursor, duration, position, style })` | Указатель, точка, рамка и подпись |
| `hideActions()` | Убрать обозначения действий |

Это методы `page.screencast`. Overlay с `pointer-events: none` не должен
перехватывать взаимодействие. Для подписи рядом с объектом получи актуальный
`locator.boundingBox()` и обработай `null`: элемент может быть скрыт.
Удаляй постоянные overlays после использования.

Длительность и плавный набор текста допустимы для понятной демонстрации.
Не переноси демонстрационные паузы в проверки синхронизации приложения.

## Передача результата

Выбирай короткий сценарий и разумный размер кадра; запись добавляет нагрузку
и занимает диск. Перед передачей просмотри файл и проверь отсутствие секретов.
Публикация в PR допустима только в рамках поручения:
[pr-attachments.md](pr-attachments.md).

Trace сохраняет DOM, сеть и действия для диагностики; видео показывает
визуальную последовательность. Ни одно не гарантирует меньшее место на диске:
это зависит от длительности и содержимого.
