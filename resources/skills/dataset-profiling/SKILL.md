---
name: dataset-profiling
description: Profile CSV, XLSX, JSONL, or Parquet datasets for missing values, duplicates, types, keys, date coverage, and join risks before analysis.
metadata:
  required_resources:
    - references/profiling-checks.md
---

# Dataset profiling

Use existing file tools and `bash` to inspect data; this workflow needs no new
harness tool. Use `doc_info` / targeted `doc_read` for an initial workbook look.
Choose checks that affect the user's question, proportional to dataset size.
For multi-file joins, large inputs, or a reusable profile, read
[profiling checks](references/profiling-checks.md) with `skill_read`.

## Establish what was loaded

Inspect raw records and workbook sheets before selecting the parser. Preserve
identifier strings, leading zeros, units, and date/time meaning. Verify delimiter,
encoding, headers, row widths, and loaded record counts. CSV records may contain
newlines, so physical line counts are not a reliable record count. Report parse
errors and skipped records; never silently discard them or compute conclusions
from an unexplained partial load.

Use available Python libraries, such as pandas/openpyxl, through the current
execution environment. For large inputs, use bounded samples or chunked scans;
label sampled estimates separately from exact whole-file counts. Do not install
an entire profiling platform for a one-off task.

## Check fitness for the question

- Identify the row grain and test proposed keys; count duplicate records and
  conflicting records sharing a key.
- Inspect inferred and semantic types, missingness, category frequencies, and
  numeric ranges. Treat apparent sentinel values as hypotheses until domain
  evidence supports interpreting them as missing.
- Check units, invalid values, date parsing ambiguity, timezone consistency,
  coverage gaps, and incomplete reporting periods that could bias comparisons.
- Before recommending a join, measure key overlap, uniqueness on each side,
  unmatched keys, and expected row multiplication.
- Report outliers as candidates for investigation. Do not delete or repair
  source values unless the user requested cleaning.

Tie material findings to counts, denominators, affected columns, and examples.
Limit expensive cross-column checks to plausible candidates; state exclusions.
Conclude which requested analyses are supported, which need preparation, and
which cannot be answered from the available data.

## Output

Answer in chat for a quick inspection. For a requested reusable profile, save
the analysis script and a compact JSON summary beside the requested report.
Record input identity, parser settings, sample coverage, and dependencies so
the profile can be reproduced. Keep source files intact and avoid displaying
unnecessary personal row values. Hand off to `spreadsheet-production` only when
the user needs a workbook deliverable.
