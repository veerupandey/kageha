---
name: modernize
description: Plan and perform incremental Python modernization with explicit verification.
---

Inspect the codebase and identify a small, reviewable migration scope.
Record the intended changes and existing behavior before editing modules.
Modernize incrementally, preserving public interfaces unless a change is requested.
Run focused checks for each changed module and report their results.
Identify any compatibility decisions or unverified behavior requiring human review.
The module status pane tracks tool activity; it does not establish test coverage
or prove a module correct. Keep verification evidence in the session's report.
