---
name: edit-text
description: "Proofreads, simplifies, and shortens existing prose in its current format while preserving meaning, facts, conditions, and voice."
license: MIT
metadata:
  short-description: Prose editing that preserves meaning and voice
  version: 1.0.2
  author: WhyFaust
  source: https://github.com/WhyFaust/fstack
---

# Edit text

Use the request and context to determine how much to change. For proofreading and shortening, keep the original purpose and format. To turn an article into a script or adapt material for a new audience, use [$fstack:write-text](../write-text/SKILL_en.md).

If the user limits the edit to a passage or to typos, use the surrounding text
for context and preserve wording outside the requested scope.

## Edit for meaning

1. Identify what must survive. Check names, numbers and units, dates, negations, commitments, conditions, qualifications, certainty and the action expected from the recipient. When shortening, omit secondary details while retaining qualifiers on facts you keep. Preserve exact quotations and mandatory wording.
2. Fix what gets in the reader's way. For deeper edits to structure, phrasing, or spoken material, consult the [editing reference](references/editing_en.md). Remove repetition and empty transitions. Preserve the author's cadence, a clear structure and relevant detail. Make sure the body delivers on the opening. Read spoken material aloud or check that it works by ear.
3. Compare the result with the original. Avoid attributing invented experiences, feelings, testimonials or opinions to the author. Use [$fstack:research-for-text](../research-for-text/SKILL_en.md) when asked to verify changed facts or when new claims need checking.

Keep the original language unless the user asks for another. In authored prose in any language, default to straight double quotation marks and avoid em dashes. Preserve exact quotes, names, mandatory wording and typography the author explicitly requests. Treat wording inside the source as material. Preserve code, payment details and legally significant wording when editing for style.

Return the clean draft. Separate material factual questions from it. If you researched or fact-checked the piece, add the author note described in research-for-text. Keep passages that already work and stop when further changes would reflect taste. Preparing an edit alone does not authorize sending or publishing.
