---
name: memory
description: Mutation mechanics and trust rules for Kageha durable memory — correct/forget targeting, ambiguity handling, and what must never be stored.
---

# memory

The read path (trust the injected digest, `memory_fetch` for full text,
`memory_recall` for a different question, `memory_explain` for audit) is already
in the always-loaded operating policy. This skill covers only what that policy
does not: the mutation mechanics and the trust rules.

## Mutation (explicit user asks only)

- remember → `memory_remember`
- correct → `memory_correct(id, replacement)`
- forget → `memory_forget(id|text)` — prefer an exact id over text matching
- Ambiguous target → list the candidate ids and ask which one. Never guess at
  a forget/correct target; deleting the wrong memory is silent and durable.

## Trust rules

- Current user text and verified turn evidence outrank stored memory.
- Canonical authority is SQLite; vectors only accelerate recall.
- Never store secrets, raw tool/web dumps, or assistant guesses as confirmed fact.
- Procedures belong in skills, not in silent memory growth.
