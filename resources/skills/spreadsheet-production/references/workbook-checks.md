# Workbook checks

## Preserve the edit target

Load an existing workbook with formulas intact (`data_only=False` in openpyxl).
Inspect cached results separately if useful. Preserve sheet names, named ranges,
tables, validation, comments, charts, hidden content, and workbook calculation
settings unless the change requires otherwise. Unsupported objects/macros may
need a different engine; determine that before saving over valuable content.
Compare affected dependencies and unrelated tabs for unintended changes.

Check available libraries before choosing a builder. Resolve missing dependencies
only in the selected task environment; a library import on the host does not
establish availability in a task machine. A local XLSX is not a silent substitute
for a requested Google Sheets or live Excel edit.

## Keep meaning in the cells

Store amounts and rates as numbers, dates as dates, identifiers as strings.
An underlying 0.15 formatted as `0.0%` represents 15%; a stored 15 does not.
Keep formatting separate from values and use engine-supported format codes.
Keep source provenance beside input data or in the workbook's established source
location; do not duplicate URLs throughout outputs. Reuse authoritative inputs
and link summaries to useful calculations rather than duplicating logic.

Handle imported text as text where it could become a formula. With openpyxl,
an explicitly textual cell can have `data_type = 's'` after setting its value.
Check after reopening that the value remains text. Treat intentional formulas
separately. CSV has different escaping behavior across applications; preserve the
requested format and explain material limitations rather than silently changing
source strings or relying on quoting as protection.

## Prove that the workbook responds

Use independent source controls for headline totals, first/middle/last copied
formulas, and representative rounding. Check row grain and periods before
aggregation: a balance is not summed over time like a flow, and combined rates
usually need recomputation from their numerators and denominators.

Recalculate in an available compatible engine, then inspect results for reference,
division, value, name, lookup, numeric, spill, and circular-reference failures.
Expected missing-data markers need an explanation. Do not use `IFERROR(...,0)`
to hide broken formulas as a healthy zero.

On a disposable copy, change a meaningful input and confirm dependent cells,
charts, and requested statuses change correctly. Exercise relevant boundaries:
blank versus zero, absent or duplicate lookup keys, period cutoff, and active
scenario/override. Input changes are useful checks only if results are actually
recalculated; rereading a stale cache proves nothing. Restore/remove temporary
edits and export the final unperturbed workbook. Recheck any conversion losses.

Inspect every sheet of a new formatted workbook and affected views of an edit
at ordinary zoom: clipped numbers/headers, `####`, blank charts, unreadable
labels, pane placement, and unexpected blank sheets. State which formula-engine,
native feature, or visual checks could not run. Keep verification scripts and
temporary audit cells out of the user workbook unless requested.
