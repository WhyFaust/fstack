---
name: glab
description: "Выполняет разрешённые операции с MR, CI, артефактами и GitLab API через glab, когда нужная возможность MCP недоступна. Сопровождение и слияние задают отдельные методы."
disable-model-invocation: true
---

# GitLab через CLI

## Вход и границы

Проверь GitLab MCP. Если операции нет или повторные тайм-ауты мешают работе,
сверь состояние, область и авторизацию по [$fstack:verify](../verify/SKILL.md),
затем используй CLI. Отказ политики или прав исключает переход и смену токена.
Сверь хост, проект, IID, ветку и ревизию, особенно при
другом проекте или нескольких remotes. Установленный синтаксис проверь через
`glab <command> --help`. Сопровождение, замечания и merge ведут
[$fstack:watch-pr](../watch-pr/SKILL.md), [$fstack:feedback](../feedback/SKILL.md)
и [$fstack:merge](../merge/SKILL.md).

## Порядок работы

### Чтение и CI

Для снимка используй `ci get` (`ci trace` ждёт, `ci view` интерактивен).
Для `ci retry` возьми ID попытки, для коллекций `glab api --paginate`.

```bash
glab mr view "$mr_iid"
glab mr list --output json
glab ci get --pipeline-id "$pipeline_id" --with-job-details
glab api "projects/:id/jobs/$job_id/trace"
```

### Изменения

Пиши в пределах поручения. При неизвестном исходе сначала прочитай результат.
В полном цикле веди один MR и его source branch по
[$fstack:watch-pr](../watch-pr/SKILL.md#полный-цикл-gitlab-mr).
Бери шаблон проекта, давай полные ссылки GitLab.

- Для разрешённого запуска сверяй проект и условия CI. Отсутствующий pipeline
  создавай через `POST projects/$target_project_id/merge_requests/$mr_iid/pipelines`,
  обязательную ручную job запускай через `POST projects/$job_project_id/jobs/$job_id/play`.
  Перечитай ID. [Pipelines](https://docs.gitlab.com/api/merge_requests/#create-merge-request-pipeline) · [Jobs](https://docs.gitlab.com/api/jobs/#run-a-job)
- Для назначений передай только нужные поля JSON `assignee_ids`, `reviewer_ids`
  через `glab api --hostname "$gitlab_host" --input` в
  `PUT projects/$target_project_id/merge_requests/$mr_iid`. Сохрани назначения
  и подтверди `assignees`, `reviewers` последующим GET.
  [API назначения](https://docs.gitlab.com/api/merge_requests/#update-a-merge-request)

```bash
glab mr create --draft --push --title "$title" --description "$description"
glab mr note create "$mr_iid" -m "$note" --unique
glab mr note create "$mr_iid" --reply "$discussion_id" -m "$reply"
glab mr note resolve "$mr_iid" "$discussion_id"
```

Используй `--push` только для ещё не опубликованной ветки. Для MR указывай
`--description`, фильтры списка `--all`, `--merged`, `--closed`.

### Текст в PowerShell

Храни Markdown в UTF-8 файле с переводами строк. Передавай содержимое без
интерполяции обратных кавычек, `$()` и многострочного текста в shell.

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Get-Content -LiteralPath $notePath -Raw -Encoding utf8 | glab mr note create $mrIid
$mrDescription = Get-Content -LiteralPath $descriptionPath -Raw -Encoding utf8
glab mr update $mrIid --description $mrDescription
glab api "projects/:id/issues/$issueIid/notes" -F "body=@$notePath"
```

Передай текст, чтобы избежать редактора и `--description "-"`. Для длинной
issue/incident note используй API с `-F body=@file`. Артефакт бери из
`projects/$project_id/jobs/$job_id/artifacts` по проекту, ID и SHA попытки.
Сверь код выхода и архив. [Документация glab](https://docs.gitlab.com/cli/)

## Проверка и результат

Сверь источник и цель MR, ID и SHA pipeline, последние jobs и дочерние
запуски. Для jobs бери project ID pipeline. У merged results проверь обоих
родителей, после смены target перечитай результат.
[Синтетический merge-коммит](https://docs.gitlab.com/ci/pipelines/merged_results_pipelines/)

Для разрешённых approve и merge дождись применимых статусов, передай полный `sha`.

```bash
glab api --hostname "$gitlab_host" --method POST \
  "projects/$target_project_id/merge_requests/$mr_iid/approve" --raw-field "sha=$source_sha"
```

Между записями перечитай MR, approvals и CI. При смене SHA проверь новую
ревизию. При merge train проследи pipeline до интеграции.
[API approvals](https://docs.gitlab.com/api/merge_request_approvals/) · [API MR](https://docs.gitlab.com/api/merge_requests/)
