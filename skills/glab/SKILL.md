---
name: glab
description: "Выполняет разрешённые операции с MR, CI, артефактами и GitLab API через glab, когда нужная возможность MCP недоступна. Сопровождение и слияние задают отдельные методы."
license: MIT
compatibility: Установленный glab с авторизацией для целевого GitLab; доступ к репозиторию и API.
metadata:
  short-description: GitLab CLI для операций, недоступных через MCP
  version: 2.1.2
  author: WhyFaust
  source: https://github.com/WhyFaust/fstack
---

# GitLab через CLI

## Вход и границы

Сначала проверь возможности и фактическую доступность GitLab MCP. Если нужной
операции нет или повторные timeout мешают её выполнению, сверив состояние,
область и авторизацию по fstack:verify-result, используй CLI. Отказ политики
или доступа не разрешает такой переход и не является основанием менять токен.
Установи хост, проект, MR, ветку и ревизию. Если задача относится к другому проекту
или есть несколько remotes, проверь целевой проект чтением перед записью. Номер MR
имеет смысл в пределах проекта. Неизвестные опции проверяй через `glab <command> --help`.

Для состояния и сопровождения MR используй fstack:watch-pr, для замечаний —
fstack:resolve-review, для явно разрешённого слияния — fstack:ship-pr.
Этот навык задаёт транспортные команды и не запускает отдельный watcher.

## Порядок работы

### Чтение и CI

`ci trace` ожидает завершения job, `ci view` интерактивен; для снимка состояния используй `ci get`.
Для повтора конкретной попытки в `ci retry` используй ID job. Коллекции API читай с пагинацией; первая страница
не является полным набором.

Подставь параметры целевого проекта и операции.

```bash
glab mr view "$mr_iid"
glab mr list --output json
glab ci get --pipeline-id "$pipeline_id" --with-job-details
glab api "projects/:id/jobs/$job_id/trace"
```

### Изменения

В полном цикле GitLab используй один MR и его source branch по
[порядку сопровождения](../watch-pr/SKILL.md#полный-цикл-gitlab-mr).
MR, комментарий, retry, approval и merge должны входить в поручение пользователя. Перед повтором записи с неизвестным исходом прочитай её состояние.
Шаблон описания бери из репозитория. В текстах используй полные ссылки GitLab.

```bash
glab mr create --draft --push --title "$title" --description "$description"
glab mr note create "$mr_iid" -m "$note" --unique
glab mr note create "$mr_iid" --reply "$discussion_id" -m "$reply"
glab mr note resolve "$mr_iid" "$discussion_id"
```

`--push` нужен, когда ветка ещё не опубликована. У готовой ветки не публикуй посторонние
изменения. У MR используется `--description`, а не `--body`; состояния списка выбираются
через `--all`, `--merged`, `--closed`. Не смешивай флаги `gh` и `glab`.

### Текст в PowerShell

Сохраняй Markdown в UTF-8 файл с настоящими переводами строк. Не интерполируй обратные
кавычки, `$()` и многострочный текст в строку shell-команды.

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Get-Content -LiteralPath $notePath -Raw -Encoding utf8 | glab mr note create $mrIid
$mrDescription = Get-Content -LiteralPath $descriptionPath -Raw -Encoding utf8
glab mr update $mrIid --description $mrDescription
glab api "projects/:id/issues/$issueIid/notes" -F "body=@$notePath"
```

Не опускай текст, если это откроет редактор. Не передавай `--description "-"`:
поддержка stdin у команд различается. Issue/incident note не читают такой pipeline;
для длинного текста используй API с `-F body=@file`.

Для коллекции используй `glab api --paginate`. Артефакт конкретной попытки
получи по `projects/$project_id/jobs/$job_id/artifacts`, сверив проект, ID
и SHA job. Проверь код выхода и целостность архива перед использованием.
Синтаксис установленной команды сверяй с `--help`. [Документация glab](https://docs.gitlab.com/cli/)

## Проверка и результат

Сверь проект, MR, текущие source и target, ID и SHA связанного pipeline, его
последние попытки jobs и дочерние pipeline. Для jobs используй project ID
самого pipeline. У merged results проверь родителей временного merge commit
против обеих ревизий. После изменения target проверь актуальность результата.
[Документация merged results pipelines](https://docs.gitlab.com/ci/pipelines/merged_results_pipelines/)

Перед разрешёнными approve и merge дождись пересчёта готовности и одобрений.
В каждую операцию передай полный проверенный source SHA параметром `sha`.
Между записями перечитай состояние MR, approvals и CI. При несовпадении SHA
проверь новую ревизию. Если проект требует merge train, проследи pipeline
поезда до фактической интеграции. [API одобрений](https://docs.gitlab.com/api/merge_request_approvals/),
[API merge requests](https://docs.gitlab.com/api/merge_requests/)
