# Источники

fstack развивает личные навыки Faust и их вспомогательные материалы.
Справки `playwright-cli` адаптированы и сокращены на русском языке из
[Microsoft Playwright CLI](https://github.com/microsoft/playwright-cli).
Исходные материалы распространяются по [Apache License 2.0](licenses/Apache-2.0.txt).

Приёмы диагностики в `refactor-code` основаны на идеях из
`caldoai/skills`, ревизия `442c29918339cc8dbf09ae27aaf17b05745465a3`,
файл `skills/untangle-codebase/SKILL.md` (Copyright (c) 2026 Maze, MIT).

Приём проверки границ доверия в справке `review-change` адаптирован из
[Cloudflare Security Audit Skill](https://github.com/cloudflare/security-audit-skill/tree/c1c8a8c1471069fb0e188eeaff69b8e8db6564a8/skills/security-audit),
ревизия `c1c8a8c1471069fb0e188eeaff69b8e8db6564a8`.
Copyright (c) 2025-2026 Cloudflare, Inc. [Лицензия MIT](https://github.com/cloudflare/security-audit-skill/blob/c1c8a8c1471069fb0e188eeaff69b8e8db6564a8/LICENSE).

При разработке `adapt-skill` использованы материалы из `agentskills/agentskills`,
ревизия `83021e2be457a9b6679148719c1f6074f0ad975e`. Спецификация распространяется
по Apache License 2.0 (`GOVERNANCE.md`), обзор, каталог клиентов и руководства
распространяются по CC BY 4.0 (`docs/LICENSE`).

Сверены 2026-11-07 документ OpenAI Developers "Build skills", справка Codex
`openai_yaml.md` из `skill-creator` и контракт вызова в документации Claude Code
"Extend Claude with skills". Рекомендации Claude Platform Docs
[Skill authoring best practices](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices)
сверены 2026-11-09 и адаптированы к каталогу fstack для Codex.

Для `adapt-skill` сверены 2026-11-09
[OpenAI API Skills](https://developers.openai.com/api/docs/guides/tools-skills),
[OpenAI Academy: Using skills](https://openai.com/academy/skills/) и
[Build skills](https://learn.chatgpt.com/docs/build-skills).
Примеры структуры и выбора ресурсов изучены по
[openai/skills](https://github.com/openai/skills/tree/49f948faa9258a0c61caceaf225e179651397431),
в том числе по `skill-creator` (Apache-2.0). Приём ревью с привязкой замечания к
строке, последствиям и способу исправления изучен по
[Terryc21/skill-reviewer](https://github.com/Terryc21/skill-reviewer/tree/3beef1246859baa101b2bcde1ee69ba1676f95ec)
(Apache-2.0). Тексты и скрипты этих навыков в fstack не перенесены.

Приёмы разделения старта, контекста и записи, изоляции Markdown и
подготовки данных перед применением изучены по трём исходникам под MIT.
[Choirboy](https://github.com/howdeploy/choirboy-prompt/tree/70232c407435e0ccd6a72cdaca55fcc5bf7893f1),
[VaultGoblin](https://github.com/WhyFaust/VaultGoblin/tree/b844efea318212f2b3dee97a1c55bf40f9a29789),
[ObsidianDataWeave](https://github.com/howdeploy/ObsidianDataWeave/tree/ddc40c68941d66943824c668ed192aced866fce8).

При проектировании обзора и шаблонов использованы идеи организации vault,
читаемых таблиц и периодического обзора.
[kepano-obsidian](https://github.com/kepano/kepano-obsidian/tree/473697347a0eaac790a6596229c741b3915f3b77),
[Minimal](https://github.com/kepano/obsidian-minimal/tree/93321bdc23fc879966e1e3aa0cfeda77012f540f),
[40 Questions](https://github.com/kepano/40-questions/tree/10e6de763e01eb00c7bd18a6cf02b90fbe1f239f).

Карта следует спецификации Obsidian.md (Copyright (c) 2024 Obsidian.md, MIT).
Для акцентов использованы три цвета Steph Ango (Copyright (c) 2023 Steph Ango, MIT).
[JSON Canvas 1.0](https://github.com/obsidianmd/jsoncanvas/blob/456f843cb293df4f4ab1763e22ccb46a80b307c8/spec/1.0.md),
[Flexoki](https://github.com/kepano/flexoki/tree/8d723bac4a9ac46adfdf99d42155286977aac72a).

Логотип и обложка созданы встроенным ImageGen для fstack. В логотипе использован
образ маскота Faust с кудрями, очками и худи; стопка модулей связывает его с проектом.

Собственные материалы fstack распространяются по [MIT](LICENSE).
