# Deck checks

## Preserve the intended design

Determine whether a supplied deck is an edit target, design template, or content
source. Content provenance alone does not imply a request to imitate its style.
Inspect the actual slide dimensions; do not assume widescreen. For template work,
render relevant source slides before editing and compare changed output slides
against them. Preserve title/body placement, typography, logos, image crops,
footers, and theme. A shared-master edit affects every slide that uses it.

Use existing placeholders and elements when editing. Replacing an entire text
frame can discard run-level emphasis and hyperlinks; inspect and update the
appropriate paragraphs/runs where those features matter. A narrow chart change
should not rebuild the deck. Work on a distinct file unless overwrite is asked.

## Design for comprehension

State the point of each slide in plain language. Use a chart when the visual
relationship matters, a table when exact comparisons matter, and a diagram when
relationships or sequence matter. Do not force every slide into the same cards
or columns. Keep source figures, units, periods, denominators, and uncertainty
consistent across text and graphics. Label illustrative data as such.

Prefer direct chart labels and readable axes. Verify series order, category
alignment, percentage scales, and displayed units. Data labels must reflect the
same values as the plotted series; a good-looking chart can still be wrong.
Keep charts native/editable when requested and supported. Choose fonts present
in the delivery environment and with coverage for the requested language.

## Check the final saved artifact

Reopen the saved file, inspect expected slides/notes and chart data, and confirm
media are embedded or reachable as intended. Render the exported PPTX itself,
not a separate HTML approximation. Inspect each new slide, each changed slide,
and slides affected by changed masters at a readable consistent scale. Check
overlaps, text wrapping, alignment, contrast, chart labels, and missing assets.
Try rewriting or rearranging crowded content before shrinking its type.

Use installed renderer help to choose supported flags. For a local LibreOffice
conversion, use a disposable profile and output directory to avoid disturbing a
running office session or overwriting the source. PDF-to-image rendering then
provides inspectable frames. Do not bypass the selected environment's policy to
make rendering work. When a renderer is missing or fails, distinguish structural
checks from unperformed visual checks in the delivery note.

Verify meaningful requirements, not arbitrary object counts. A render establishes
appearance in that engine, not editability or identical behavior in every office
application. A successful export alone establishes neither.
