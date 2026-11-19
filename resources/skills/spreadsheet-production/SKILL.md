---
name: spreadsheet-production
description: Create or edit Excel XLSX workbooks with formulas, tables, charts, formatting, and recalculation checks, preserving unrelated sheets and cells.
metadata:
  required_resources:
    - references/workbook-checks.md
---

# Spreadsheet production

Use `doc_info` / targeted `doc_read` to inspect existing sheets. For formulas,
charts, formatting, and precise edits, use openpyxl or another available library
through `bash`. `doc_write` is useful for a basic table export, but its text-table
input is insufficient for a structured financial model or polished workbook.
For formula-driven workbooks, existing-file edits, or final verification, read
[workbook checks](references/workbook-checks.md) with `skill_read`.

## Establish the workbook contract

Inspect sheet names, ranges, tables, formulas, named ranges, data types, and
requested output before editing. Preserve the original file and unrelated cells.
Preserve macros when required and supported; do not silently strip unsupported
features. Treat identifier codes as strings, dates as dates, and measurements as
numeric values with explicit units and appropriate number formats.

Use `dataset-profiling` when analysis depends on unresolved data-quality issues.
Keep missing values distinct from zero and document material assumptions.
Separate inputs, calculations, and results when that improves usability; do not
impose a multi-sheet model on a simple table request.

## Build and edit

Use formulas for values that should update when inputs change. Verify relative
and absolute references, range boundaries, lookup behavior, and zero-denominator
cases. Protect user-supplied text that starts with formula syntax from accidental
execution, while retaining intentional formulas. Add readable headings, useful
column widths, number formats, and filters/frozen panes where they help.
Tie chart series and labels to the intended data ranges.

## Validate before delivery

Reopen the saved workbook and inspect affected formulas, cell types, ranges,
sheet names, and formatting. Reconcile totals and representative results with
an independent calculation from the input data, including meaningful edge cases.

openpyxl writes formulas but does not evaluate them. A recalculation flag is not
evidence of computed results, and `data_only=True` may return missing or stale
caches. Recalculate using an available spreadsheet engine, on a copy, and check
for formula errors and agreement with independent results. Reopen after
recalculation to check for lost features. If no engine is available, state that
formula results remain unverified; do not replace requested formulas with
hardcoded numbers to conceal the limitation.

For a formatted workbook, inspect changed sheets/charts using an available
renderer or spreadsheet app; report when visual inspection was unavailable.
Deliver the requested editable XLSX with a concise note about assumptions and
checks. Avoid exporting extra formats or building new harness tools unless the
task requires them.
