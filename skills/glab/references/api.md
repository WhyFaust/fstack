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

## Pipeline и готовность к merge

Сверь проект, MR, source head, target head и связанный pipeline: его ID, ref,
source и SHA. Branch pipeline или обычный MR pipeline может проверять source SHA,
а merged results pipeline проверяет временный merge commit. Для него установи
связь с MR и родителями commit, соответствующими проверяемым source и target;
не требуй равенства pipeline SHA и source SHA. Если target изменился после
проверки, прежний результат не доказывает совместимость с новым target.
Несовпадение SHA само по себе не повод создавать ещё один pipeline.

После изменения веток GitLab пересчитывает mergeability асинхронно. Наблюдай
`detailed_merge_status` ограниченное время; `checking` или `preparing` не означают
готовность. `with_merge_status_recheck` запрашивает пересчёт, но не гарантирует
завершение: не добавляй его к каждому опросу. При явно разрешённом merge передай
проверенный source head в параметре `sha`; это защита от смены source, а не SHA
временного merge commit. Неизвестный исход записи сначала установи чтением MR.

Контракты: [merged results pipelines](https://docs.gitlab.com/ci/pipelines/merged_results_pipelines/)
и [Merge requests API](https://docs.gitlab.com/api/merge_requests/).

## Обсуждения

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
