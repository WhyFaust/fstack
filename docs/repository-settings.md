# Настройки GitHub вне файлов пакета

PR меняет код и workflow. Правила защиты ветки включаются отдельно в настройках
GitHub администратором; файл в репозитории не активирует их сам по себе.

## Защита основных веток

[Декларация ruleset](../.github/release-branches.ruleset.json) запрещает удаление
и force-push основной ветки и исторических `marketplace/*`.
Список bypass пуст: правило распространяется и на администратора. Владелец
репозитория по-прежнему может изменить сами настройки GitHub; это защита от
случайной операции с веткой, а не лишение владельца административных прав.
Workflow публикует тег и артефакты GitHub Release.

Перед применением прочитай существующие правила и выбери ID подходящего
ruleset. Не заменяй более строгие или несвязанные правила этой декларацией.
В PowerShell для нового правила:

```powershell
gh api repos/WhyFaust/fstack/rulesets
gh api --method POST repos/WhyFaust/fstack/rulesets --input .github/release-branches.ruleset.json
```

Для обновления существующего используй `--method PUT` с адресом
`repos/WhyFaust/fstack/rulesets/ID`. Применение требует отдельного разрешения
на настройки GitHub. Проверь ответ: `enforcement: active`, условия веток,
оба запрета и отсутствие bypass. Затем прочитай применённые правила:

```powershell
gh api repos/WhyFaust/fstack/rules/branches/main
gh api repos/WhyFaust/fstack/rules/branches/marketplace%2Fx86_64-pc-windows-msvc
gh api repos/WhyFaust/fstack/branches --paginate
```

Проверяй каждую существующую платформенную ветку; намеренно удалять или
переписывать её для проверки не нужно. `enforcement: disabled` и наличие
декларации в Git сами по себе не защищают ветки.
[Контракт REST API rulesets](https://docs.github.com/en/rest/repos/rules).

## Проверяемый контракт

| Параметр | Требование |
| --- | --- |
| Рабочая ветка | Изменения через отдельные ветки и PR |
| Проверки PR | Все jobs `package` из актуальной матрицы [workflow](../.github/workflows/release.yml) для текущего head/merge snapshot |
| История main | Без force-push и удаления |
| Метод слияния | Squash с проверенным заголовком PR или merge-коммит, сохраняющий историю ветки |
| Workflow token | contents: read для проверок; contents: write только для release |
| Релиз | Версия зарезервирована в PR. Workflow публикует проверенную сборку в GitHub Releases |
| Ветки `marketplace/*` | Исторические снимки для миграции. Релиз больше их не обновляет |

При настройке required checks копируй точные имена jobs из последнего прогона
workflow. После изменения матрицы сверяй этот список: прежнее имя проверки
не получает статус от нового job и может заблокировать слияние.

## Совместимость защиты с релизом

Включи обязательную проверку матрицы для актуальной базы PR и требование
«Require branches to be up to date before merging». Так второй из двух PR с
одинаковой версией повторно проверится на новой main и рассчитает следующий
номер. При squash сохрани проверенный Conventional Commit заголовок PR.
При rebase каждый коммит по отдельности попадает в историю main и должен
содержать собственную версию. Для многокоммитного PR с общей версией
выбери squash или обычный merge. Публикация не записывает коммиты в main, поэтому защита ветки не требует
служебного bypass. Эти настройки меняет администратор репозитория.
