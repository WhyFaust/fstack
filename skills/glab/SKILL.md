---
name: glab
description: 'Выполняет операции GitLab через glab, когда нужной возможности нет в доступном MCP. Используй для MR, CI, артефактов и API с существующей авторизацией; сопровождение и слияние задаются отдельными методами.'
license: MIT
compatibility: Установленный glab с авторизацией для целевого GitLab; доступ к репозиторию и API.
metadata:
  short-description: GitLab CLI для операций, недоступных через MCP
  version: 2.1.0
  author: WhyFaust
  source: https://github.com/WhyFaust/fstack
---

# GitLab через CLI

Сначала проверь возможности и фактическую доступность GitLab MCP. Если нужной
операции нет или повторные timeout мешают её выполнению, сверив состояние,
область и авторизацию по fstack:verify-result, используй CLI. Отказ политики
или доступа не разрешает такой переход и не является основанием менять токен.
Установи repository, MR, ветку и ревизию. Неизвестные опции проверяй через `glab <command> --help`.

Для состояния и сопровождения MR используй fstack:watch-pr, для замечаний —
fstack:resolve-review, для явно разрешённого слияния — fstack:ship-pr.
Этот навык задаёт транспортные команды и не запускает отдельный watcher.

## Чтение и CI

```bash
glab mr view 123
glab mr list --output json
glab ci get --pipeline-id 456 --with-job-details
glab api projects/:id/jobs/789/trace
```

Проверяй принадлежность pipeline нужной ревизии MR и последнюю применимую попытку
jobs; для merged results связь устанавливай по [справке API](references/api.md).
`ci trace` ожидает завершения job, `ci view` интерактивен; для снимка состояния используй `ci get`.
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

Для пагинации JSON, скачивания артефактов, placeholder API, diff comments,
merged results pipelines и mergeability прочитай [справку API](references/api.md);
обычным командам выше она не нужна.
Источник: [документация glab](https://docs.gitlab.com/cli/).
