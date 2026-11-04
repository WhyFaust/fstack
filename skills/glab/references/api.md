# API и обсуждения GitLab

`glab api` добавляет `/api/v4/`: передавай `projects/...`. Внутри clone `:id`,
`:fullpath`, `:repo`, `:group`, `:namespace`, `:branch` берутся из контекста;
`:user` и `:username` доступны отдельно. Остальные слова после двоеточия не являются
placeholder. Для чужого проекта укажи ID или URL-encoded path.

```bash
glab api projects/group%2Fproject/merge_requests --paginate
glab api projects/:id/issues/123/notes -F body=@work/comment.md
glab api projects/:id --method PUT -F 'topics=["codex","skills"]'
glab api projects/:id/merge_requests/123/notes --method POST --input work/body.json -H "Content-Type: application/json"
```

- `-f` передаёт буквальную строку, включая начальный `@`.
- `-F body=@file` читает файл как текстовое поле; это не загрузка бинарного файла.
- `-F` распознаёт JSON-массивы и объекты. Не заменяй массив формой `ids[]=1`.
- `--input` передаёт сырой JSON; добавь `Content-Type: application/json`.
- `--form` нужен для multipart, несовместим с `-f`, `-F`, `--input`.

```bash
glab api projects/:id/uploads --method POST --form "file=@work/screenshot.png"
glab api projects/:fullpath/wikis/attachments --method POST --form "file=@work/screenshot.png" --form "branch=main"
```

Для diff comment проверь текущую diff version. `--line` указывает новую сторону,
`--old-line` — старую; оба требуют `--file`, но несовместимы между собой.

```bash
glab mr note create 123 --file src/main.py --line 42 -m "Описание дефекта"
glab mr note create 123 --file src/main.py --old-line 7 -m "Описание удалённой строки"
glab mr note create 123 --reply discussion-id -m "Ответ"
glab mr note reopen 123 discussion-id
```

`--file`, `--reply`, `--unique` взаимоисключающие. Экспериментальные флаги сверяй
с `--help`. `--resolvable=false` подходит для разрешённой информационной записи,
но не для скрытия замечания, которое должно блокировать merge. Для вложенных
issue/work-item discussions при отсутствии CLI команды используй API; ID и
принадлежность проекту установи чтением перед записью.
