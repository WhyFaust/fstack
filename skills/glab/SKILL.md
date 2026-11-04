---
name: glab
description: Выполняет необходимую операцию с GitLab через CLI, когда GitLab MCP не предоставляет её или
  недоступен. Используй для репозиториев, MR, CI, артефактов и API; сохраняй авторизацию задачи и безопасную
  передачу многострочного текста.
metadata:
  short-description: GitLab CLI для операций, недоступных через MCP
  version: 0.1.3
---

# GitLab через CLI


Сначала проверь возможности GitLab MCP. CLI — резервный способ выполнить операцию,
а не основание менять токен или обходить отказ в доступе. Установи repository, MR,
ветку и ревизию. Неизвестные опции проверяй через `glab <command> --help`.

## Чтение и CI

```bash
glab mr view 123
glab mr list --output json
glab ci get --pipeline-id 456 --with-job-details
glab api projects/:id/jobs/789/trace
```

Проверяй jobs именно нужного SHA и последней применимой попытки. `ci trace` ожидает
завершения job, `ci view` интерактивен; для снимка состояния используй `ci get`.
Аргумент `ci retry` — ID job. Коллекции API читай с пагинацией; первая страница
не является полным набором.

## Изменения

MR, комментарий, retry, merge и изменение данных должны входить в авторизацию
пользователя. Перед повтором записи с неизвестным исходом прочитай её состояние.
Шаблон описания бери из репозитория. В текстах используй полные ссылки GitLab.

```bash
glab mr create --push --title "fix: исправить обработку ответа" --description "..."
glab mr note create 123 -m "Результат проверки" --unique
glab mr note create 123 --reply discussion-id -m "Ответ"
glab mr note resolve 123 discussion-id
```

`--push` нужен, когда ветка ещё не опубликована. У готовой ветки не публикуй посторонние
изменения. У MR используется `--description`, а не `--body`; состояния списка выбираются
через `--all`, `--merged`, `--closed`. Не смешивай флаги `gh` и `glab`.

## Текст в PowerShell

Сохраняй Markdown в UTF-8 файл с настоящими переводами строк. Не интерполируй обратные
кавычки, `$()` и многострочный текст в строку shell-команды.

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Get-Content -LiteralPath '.\work\comment.md' -Raw -Encoding utf8 | glab mr note create 123
$mrDescription = Get-Content -LiteralPath '.\work\description.md' -Raw -Encoding utf8
glab mr update 123 --description $mrDescription
glab api projects/:id/issues/123/notes -F 'body=@work/comment.md'
```

Не опускай текст, если это откроет редактор. Не передавай `--description "-"`:
поддержка stdin у команд различается. Issue/incident note не читают такой pipeline;
для длинного текста используй API с `-F body=@file`.

Для JSON, загрузок, placeholder и diff comments прочитай [справку](references/api.md).
Источник: [документация glab](https://docs.gitlab.com/cli/).
