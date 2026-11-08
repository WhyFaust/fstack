# Команды памяти

Запускай [memory.py](../scripts/memory.py) из каталога подключённого проекта.
Путь ниже обозначает файл в установленном навыке. На системах, где Python 3
доступен как `python3`, используй это имя вместо `python`.

```text
python /path/to/memory.py status
python /path/to/memory.py search "условия выпуска" --limit 5
python /path/to/memory.py show ID
python /path/to/memory.py inbox
python /path/to/memory.py disable
```

## Подключение

Выполни команду только с явно выбранным vault, рабочей папкой и идентификатором
проекта. Vault должен содержать каталог конфигурации `.obsidian`. Git worktree
одного репозитория используют одну привязку. Для папки вне Git привязка действует
из этой папки. Для `init` возьми Python, установленный вне рабочей папки.
Хуки будут использовать этот интерпретатор. После смены Python повтори настройку.

```text
python /path/to/memory.py init --vault "/path/to/vault" --repo "/path/to/project" --project my-project --mode auto
```

Режим `auto` применяет подтверждённые записи, а режим `inbox` направляет
кандидатов в Inbox. Открой новый чат и одобри хуки fstack в меню `/hooks`
или через **Settings → Hooks** в Codex Desktop. Подключение локально для
устройства. Используй одинаковый `--project` на каждом устройстве, где
работаешь с этой памятью.

## Запись из чата

Передай один объект JSON в `pack` через стандартный ввод в UTF-8. Команда
вернёт строку base64url в JSON. Возьми содержимое строки без JSON-кавычек
и вызови отдельную команду в той же рабочей папке.

```bash
"/absolute/path/to/python3" -E -s -B "/absolute/path/to/memory.py" stage --turn TOKEN --payload BASE64URL
```

На Windows используй форму PowerShell.

```powershell
& "C:\absolute\path\to\python.exe" -E -s -B "C:\absolute\path\to\memory.py" stage --turn TOKEN --payload BASE64URL
```

Возьми готовый префикс команды и токен из контекста хука. Подставь только
строку из `pack`. Хук принимает один вызов без пайпа, перенаправления
и других команд.
Результат `{"staged": true}` означает, что команда прошла проверку.
Запись применяет `Stop`. Токен и base64url предназначены для вызова
инструмента, в ответе пользователю их не повторяй.

Пример объекта для `pack`

```json
{
  "action": "remember",
  "candidate": {
    "kind": "decision",
    "title": "Условия публикации пакета",
    "body": "Публикуем только из main после прохождения матрицы Windows и Ubuntu."
  }
}
```

`action` принимает `remember` для подтверждённого факта или `draft` для
кандидата. `kind` принимает `decision`, `preference` или `correction`.
В `candidate` можно передать `source` со ссылкой и `supersedes`
с идентификатором предыдущей записи. Для обработки Inbox и удаления передай
`{"action":"approve","id":"ID"}` или `{"action":"forget","id":"ID"}`.

В терминале владелец может вызвать `remember` с объектом `candidate` через
stdin, `approve ID` и `forget ID` напрямую. `remember` учитывает режим
привязки. Записи и Inbox относятся к привязке текущей папки. `disable`
снимает её на этом устройстве.
