---
name: presentation-production
description: Create or edit an editable PowerPoint PPTX presentation or slide deck, preserving supplied templates and checking the rendered layout.
metadata:
  required_resources:
    - references/deck-checks.md
---

# Presentation production

Use existing file, shell, document, and image tools. Inspect an input deck with
`doc_info` and targeted `doc_read`, then use python-pptx or an available authoring
library through `bash` for richer layout and edits. `doc_write` is suitable for
a simple heading/bullet draft, not a substitute for intentional slide design.
For a supplied template, chart-heavy deck, or final layout review, read
[deck checks](references/deck-checks.md) with `skill_read`.

## Plan and build

Honor the requested audience, purpose, format, template, and brand. Infer ordinary
layout choices from supplied material; ask only when missing context changes
the message. Reuse already gathered research. Load `evidence-synthesis` only if
the task actually requires further cross-source work.
For an outline-only request, provide the outline without running authoring or
rendering. Keep an established output destination; do not silently substitute a
local file for an explicitly requested connected-app edit.

Give each slide a clear point and choose a layout that communicates it: chart,
comparison, annotated image, sequence, or concise text. Prefer native editable
text, shapes, tables, and charts. Use raster images for visual assets; do not
flatten every slide into a screenshot when an editable PPTX was requested.
Keep sources with factual charts/claims and detailed support in speaker notes
when appropriate. Never invent metrics or imply placeholder data are real.

For revisions, inspect existing masters, dimensions, theme, and relevant slides.
Preserve unrelated content and the user's original file; write the revision to
a distinct output unless overwrite was requested. Keep edits to the requested
scope and report any library limitation that affects fidelity.

## Verify the deliverable

Reopen the saved PPTX and check slide count, expected content, linked assets,
chart values, and editability. Render with an available office renderer to PDF
or images, then visually inspect every changed slide for clipping, overlap,
contrast, readable labels, font substitution, and excessive density. Inspect all
slides in a new deck. Repair affected slides and recheck those outputs.

Use installed tools in the selected execution environment. If rendering is
unavailable, deliver the structurally checked PPTX and state that visual layout
was not verified; do not claim a screenshot or text extraction proves it.
An HTML/PDF companion may be useful when requested, but it is a distinct output
and does not verify the PPTX rendering.

Return the finished file with a brief description of changes and verification.
Keep editable sources/assets when they are part of the requested handoff. Avoid
adding a new artifact service, renderer daemon, or harness tool for this workflow.
