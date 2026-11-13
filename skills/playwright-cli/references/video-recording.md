# Запись видео

Видео WebM показывает пользовательский сценарий; оно дополняет проверки,
но не заменяет assertions, сетевые доказательства и trace.

## Короткая запись

```bash
playwright-cli -s=demo open "$url"
playwright-cli -s=demo video-start demo.webm --cursor --fps=60
playwright-cli -s=demo video-chapter "$title" --description="$description" --duration=2000
playwright-cli -s=demo snapshot
playwright-cli -s=demo click "$element_ref"
playwright-cli -s=demo video-stop
playwright-cli -s=demo close
```

Используй ссылку элемента из свежего снимка. Проверь путь к записи по выводу CLI.
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

Запусти запись через `await page.screencast.start({ path, size, fps })`
до первого действия. В блоке `try` выполни проверяемые шаги, а в `finally`
вызови `await page.screencast.stop()`. Если завершение записи тоже упало,
сохрани обе ошибки.

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
