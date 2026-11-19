# Profiling checks

Use only the checks that affect the question. Inspect available libraries before
choosing an implementation. Standard-library csv/json/sqlite3 can cover small
files; use available dataframe/Parquet readers for larger data. If a dependency
is missing, resolve it in the selected task environment or report the specific
format limitation. Do not send private data to an external service to parse it.

## Coverage and parsing

Record input path or table/query identity, file hash when practical, size,
encoding, delimiter, selected sheet/range, parser settings, and excluded rows.
Keep raw data intact. Interpret missingness and type conversion separately:
unparseable amounts must not become valid-looking zeroes.

Report scanned versus total records and how the sample was chosen. A head sample
can miss later schema changes, groups, or seasonal behavior. Quantiles estimated
from samples need that qualification. Exact uniqueness and join cardinality
require complete keys or an exact database query; sample-level uniqueness is
only a candidate-key suggestion. Distinct estimates are not proven keys.

For workbook formulas, distinguish formula text from cached values; absent or
stale caches are not missing business inputs. Identify the observation rather
than treating cached output as freshly calculated data.

## Checks that change the conclusion

For each material column, record semantic type, parse failures, missing count
and denominator, distinct count/estimate, and relevant range/frequencies. Include
units and currency. Determine whether a row represents a person, order, line
item, event, or aggregate before totaling it. Test the proposed grain directly.

Compare missingness and record volume across relevant groups/time periods when
they could bias a result. State calendar and timezone assumptions. Separate an
incomplete final period from a genuine decline. Avoid unbounded pairwise scans.

Before a join, check key normalization, null behavior, uniqueness on both sides,
unmatched keys, and predicted multiplicity. For an equality inner join, each
matching key contributes left_count × right_count rows; sum those products.
State how null keys are handled: pandas and SQL can differ. For example, two
records for a key on the left and three on the right produce six joined rows,
even when every key appears in both files.

## Reusable result

When a reusable profile is requested, write one rerunnable script and a compact
summary containing input/coverage, parser, row grain, column diagnostics, tested
keys, join diagnostics, and findings. Use null plus an explanation for unknown
results; JSON must not contain NaN/Infinity. Tie findings to measured evidence
and the requested analysis. Hash or compare inputs after the run when accidental
mutation is a concern. Do not generate a large report for a quick question.
