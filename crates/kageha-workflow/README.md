# Evidence-driven Rust workflow controller

`kageha-workflow` is a framework-independent completion controller. It has no
browser, application, provider, HTTP client, Python, or Workday dependency. It
uses the portable model interface from `kageha-contracts`.

The host supplies an immutable versioned `Spec`, a trusted `Observation`, and a
replaceable `Store`. The controller returns `proceed`, `continue`, `verified`, or
`stop`. Model prose is never completion evidence. Every required goal and the
absence of unresolved action receipts must be verified in the pinned resource
and origin. Cached completion is revalidated on every checkpoint.

`GuardedModel` wraps any `ModelPort`: premature final answers are rejected inside
the same request, not published as successful user-facing answers. The next
model call receives the controller's unmet-outcome instructions. Continuation,
no-progress and total-checkpoint budgets are durable and bounded. A trusted
adapter can produce an `awaiting_input` blocker for missing facts, permission or
user-held control. Cancellation is honored before further model calls.
Repeated tool calls also consume a separate no-progress model-call budget; a
model cannot evade the limit by avoiding a final answer.

`BeforeMutation` is a second gate for the tool adapter. After success, no further
mutation is authorized. The tool adapter still owns actual control identity,
side-effect classification, permissions, receipts, and commit restrictions; the
workflow controller does not bypass or replace those controls.

## Plug in

- Rust: implement `Port` and `Store`, or call `evaluate` with caller-owned state.
- Optional persistence: enable `sqlite` and use `sqlite::SqliteStore`; state writes
  are revision-fenced and journaled. The default feature set requires no database.
- Other languages: run `kageha-workflow`, send one JSON frame per line containing
  `spec`, `previous`, `observation`, and `checkpoint`, and persist the returned
  `state`. `--describe` reports the protocol descriptor.
- API hosts: wrap the same `evaluate` function. Authentication and the provenance
  of evidence belong to the host. Never expose an endpoint that lets a model
  self-certify its observation as trusted evidence.

## Kageha browser adapter

The native host accepts an optional `workflow` contract on `POST /api/chat/send`:

```json
{
  "version": 1,
  "profile": "comet",
  "target_id": "existing-managed-tab",
  "origin": "https://employer.example",
  "known_context": "Caller-provided answers and permissions; never credentials",
  "goals": [
    {
      "id": "review",
      "description": "Draft is at Review; final submission is excluded",
      "selector": "h1,h2,h3",
      "equals": "Review"
    },
    {
      "id": "resume",
      "description": "The original resume remains attached",
      "selector": "div",
      "text": "resume.pdf",
      "contains": "resume.pdf",
      "depends_on": ["review"]
    }
  ]
}
```

The adapter checks exact visible DOM matches, independently of the model's
summary, and keeps fresh observations in the ordinary browser tool cache.
`equals` is preferable for question/answer rows; it cannot accept a matching
answer from a different question elsewhere in a larger container. Whitespace
and required-field stars are normalized. Rules can depend only on earlier rules.
Selectors are bounded by the existing browser inspector's limits; an unobserved
match is incomplete evidence, never a successful fallback.

The contract is pinned with the request, including during runtime-service
execution. Submit/Withdraw/Delete controls are excluded, Enter commits are
blocked, and new tabs, account transports and unconfigured tools are unavailable
inside this browser-draft adapter. Those are adapter policies, not assumptions in
the generic controller. The selected profile cannot silently change. A bound tab
can be reselected without navigating it while the agent owns control.
Pending original receipts are reconciled automatically against fresh state when
the native verifier supports the action. This never repeats page input or an
upload. Unverifiable outcomes remain pending; recovery metadata is journaled.
An upload reservation also prevents a second attempt with the same bytes at the
same destination in one workflow, including after a crash, locator alias or file
rename. This is a conservative attempt guard, not proof of attachment. Replacing
an attachment after an unverifiable attempt requires an explicit new caller
request; the model cannot grant itself that authority.

`GET /api/sessions/{id}/workflow` returns the latest request's workflow state.
Events use `workflow_checkpoint`. SQLite data lives in `components/workflow.sqlite`,
separate from historical chats. Existing requests without a workflow contract
retain their existing behavior. Natural-language goals are not silently turned
into model-authored success criteria.

## Verification

```sh
cargo test -p kageha-workflow --features sqlite
KAGEHA_LOOP_BIN=/absolute/kageha-loop KAGEHA_BROWSER_BIN=/absolute/kageha-browser \
  node crates/kageha-loop/tests/workflow_e2e.mjs
```

The local six-step browser test deliberately emits premature model finals and
uses real Chromium, generic dropdowns, a hidden checkbox/radio, a segmented date,
a consumed file input and sticky navigation. It sends one chat request, no
corrective messages and no approval replies during execution, and asserts one
upload and zero submissions. `--real` uses `KAGEHA_TEST_MODEL`,
`KAGEHA_TEST_PROVIDER_URL`, and `KAGEHA_TEST_API_KEY` for a real provider run.
`--overshoot` additionally attempts another mutation in the same model reply
that reaches Review, and verifies that the boundary gate rejects it.
`--duplicate-upload` attempts to upload the same file again using a different
selector and verifies that only the original upload reached the page.

The independent HTTP model adapter retries temporary HTTP 429/5xx responses up
to four times per provider, honors numeric Retry-After up to 60 seconds, and
checks cancellation while waiting. Larger or unsupported Retry-After values
fail closed instead of retrying early. This policy retries model requests only.

These tests establish the covered contract and flows. They do not establish that
every website, arbitrary free-form objective or authentication challenge can be
completed without human input.

## Reviewed planning component

`planning` extracts the existing WebUI's plan validation, dependency checks,
markdown/board projection, required-outcome preservation and progress rules into
this library. It has no UI types or application database imports. Hosts implement
`planning::Store` and `planning::EvidencePort`; `planning::Service` owns proposal,
exact revision/digest approval and evidence-backed progress semantics. A store must
atomically compare revisions and save immutable operation receipts. Exact retries
return the original record; changed identity reuse and stale writes fail.

The optional `planning::sqlite::SqliteStore` uses a dedicated, private plan journal
and exclusive writer lease. `cargo run -p kageha-workflow --example planning` uses
a caller-owned memory adapter instead, without enabling SQLite or starting Kageha.
The service checks adapter scope and rejects incompatible returned records.

Proposals reset model-provided approval, step completion and outcome claims.
Required IDs/text cannot disappear in revisions, approved designs are locked,
steps honor dependencies, and completion needs successful tool-call references
validated by the host in the correct scope. Optional `outcome_ids` associates a
completed step's evidence with required outcomes. Reopening supporting work
invalidates those outcome claims. A successful tool receipt is a fact, not universal
proof of an external transaction or semantic satisfaction of acceptance criteria;
applications needing that proof use the observation-based workflow controller.

The WebUI adapter retains its existing chat data and API, delegating domain rules
to this module. Standalone runtime has its own journal and approval/evidence
adapters. Migrating every legacy planning/task persistence format into one owned
store and managed live replacement remain separate release requirements.
