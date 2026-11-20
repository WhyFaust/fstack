---
name: glab
description: "Работает с GitLab через glab, когда нужная операция недоступна через MCP."
disable-model-invocation: true
metadata:
  version: "2.2.5"
---

# GitLab через CLI

[Каталог · Ревью и доставка](../../docs/skills.md#ревью-и-доставка)

Цитата не включает навык. Вне faust-mode применяй только явно вызванные навыки.

Если нужной операции GitLab MCP нет или повторные таймауты мешают работе,
сверь текущее состояние, полномочия, хост, проект, IID и ревизию перед переходом
к CLI. Отказ политики или прав исключает смену клиента или токена. При отказе
Guardian [останови серию](../watch-pr/SKILL.md#остановить-серию-записей-при-отказе).
Установленный синтаксис проверь через `glab <command> --help`.

## Чтение и CI

В `get_merge_request` передавай одно значение `include` за вызов. Для
нескольких видов данных сделай отдельные чтения того же MR и SHA. Для
снимка CI используй `ci get`, для retry — ID попытки, для списков API — `--paginate`.

```bash
glab mr view "$mr_iid"
glab mr list --output json
glab ci get --pipeline-id "$pipeline_id" --with-job-details
glab api "projects/:id/jobs/$job_id/trace"
```

## Изменения

В [полном цикле](../watch-pr/SKILL.md#полный-цикл-gitlab-mr) веди один MR
и source branch. Используй шаблон проекта. При неизвестном исходе перечитай
результат до повтора. `--push` нужен для ещё не опубликованной ветки.

```bash
glab mr create --draft --push --title "$title" --description "$description"
glab mr note create "$mr_iid" -m "$note" --unique
glab mr note create "$mr_iid" --reply "$discussion_id" -m "$reply"
glab mr note resolve "$mr_iid" "$discussion_id"
```

- Сверив проект и право на запуск, создай отсутствующий MR pipeline через
  `POST projects/$target_project_id/merge_requests/$mr_iid/pipelines`, запусти
  обязательную ручную job через `POST projects/$job_project_id/jobs/$job_id/play`.
  Подтверди ID запущенных pipeline и job.
- Для назначения передай нужные `assignee_ids` или `reviewer_ids` через
  `glab api --hostname "$gitlab_host" --input` в `PUT projects/$target_project_id/merge_requests/$mr_iid`.
  Сохрани действующие назначения, сверь оба поля через GET.

## Текст в PowerShell

Храни Markdown в UTF-8 с переводами строк. Читай файл буквально, чтобы
обратные кавычки и `$()` не интерполировались оболочкой.

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Get-Content -LiteralPath $notePath -Raw -Encoding utf8 | glab mr note create $mrIid
$mrDescription = Get-Content -LiteralPath $descriptionPath -Raw -Encoding utf8
glab mr update $mrIid --description $mrDescription
glab api "projects/:id/issues/$issueIid/notes" -F "body=@$notePath"
```

## Проверить состояние на сервере

Сверь source/target, ID, project и SHA pipeline, последние jobs и downstream.
Для jobs бери project ID pipeline. Артефакт получи из
`projects/$project_id/jobs/$job_id/artifacts` по SHA и ID попытки, проверь архив.
Для merged results сверь обоих родителей синтетического merge-коммита. При смене target перечитай результат.

Для разрешённых approve и merge передай полный `sha` после проверки готовности.

```bash
glab api --hostname "$gitlab_host" --method POST \
  "projects/$target_project_id/merge_requests/$mr_iid/approve" --raw-field "sha=$source_sha"
```

Между записями перечитай MR, approvals и CI. При смене SHA проверь новую
ревизию, при merge train дождись интеграции.
