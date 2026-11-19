---
name: evidence-synthesis
description: Build a claim-by-source evidence matrix or literature review, reconcile contradictory research findings, and identify supported conclusions and gaps.
triggers:
  - evidence matrix
  - contradictory findings
  - literature review
metadata:
  required_resources:
    - references/evidence-records.md
---

# Evidence synthesis

Build on `research_run`, `parallel_web_fetch`, and targeted document reads. Use
the existing `web_research` skill when retrieval needs its depth/routing guidance.
Use supplied sources first when the user limits the evidence set.
For a formal matrix, literature review, or contested conclusion, read
[evidence records](references/evidence-records.md) with `skill_read`.

## Frame and gather

Translate the question into decision-relevant claims or comparison dimensions.
Identify scope, timeframe, population, and relevant definitions. Start with a
small source set; expand when a material question remains unanswered or sources
conflict. Respect the run's budget and requested depth. Multiple articles based
on the same underlying study or announcement are one evidence lineage.

## Connect claims to evidence

Keep a compact working matrix with claim, source URL or file/page, relevant
finding, date, scope/method, stance (supports, challenges, or inconclusive), and
limitations. Capture specific passages or numerical results where needed, within
quotation limits. A search snippet is a lead, not proof of a detailed claim.
If the source cannot be read, mark that limitation rather than imply access.
For consequential conclusions, seek plausible opposing or null evidence. Record
when none was found; do not manufacture disagreement or force equal weight.

Check disagreements for different definitions, dates, samples, measurement
methods, and independence. Prefer original studies, official records, or direct
documentation for claims about their own contents. Vendor claims describe
intended behavior until independent or observed evidence establishes performance.
Do not treat the number of agreeing pages or models as confidence calibration.

## Resolve and stop

Use a focused follow-up search or document range for a consequential unresolved
claim. Stop when the requested dimensions have adequate support, further
retrieval is unlikely to change the answer, or the budget is reached. Leave
unresolved claims explicit; do not fill evidence gaps with invented consensus.
A literature synthesis is not a systematic review unless its search, screening,
and inclusion protocol actually supports that label.

## Deliver

Lead with what the evidence supports and how it affects the user's decision.
Cite the sources beside their associated claims, including file/page locations
for local material. Separate observed facts, source claims, and your inference.
Mention remaining uncertainty where it changes the conclusion.

Keep the matrix in the working context for a short task. Export it as CSV or
JSON only when requested or useful for the requested research deliverable.
For a deck or workbook, reuse this evidence through `presentation-production`
or `spreadsheet-production`; do not repeat completed retrieval merely to change
the output format.
