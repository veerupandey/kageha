# Shared policy decisions and approval ports

`kageha-contracts::policy` defines policy/approval ports and an authority scope
containing session, turn, logical binding, generation, resource and exact input
digest. `kageha-policy` supplies the default evaluation and enforcement used by
Kageha and its independent runtime service. It has no UI, application database,
browser vendor or runtime dependency.

Default behavior preserves the existing application:

- Read-only operations are allowed.
- Routine fills can use an operator-issued routine/full permission mode.
- Mutations, uploads and plans require an explicit approval adapter.
- Plan decisions and denials cannot broaden permission scope.
- Cancellation/deadline checks apply before and after an approval callback.

The host supplies trusted operation classifications. Model arguments cannot choose
a permission mode or classify an arbitrary tool as read-only. The standalone host
checks its exposed tool catalog and defaults to allowing its configured read-only
workspace reader. Other operations need an operator-supplied policy/approval port.

`ApprovalPort::authorize_once` is responsible for durable decision storage,
exact-request consent and late-decision fencing. Kageha implements it using the component-owned journal, retained active-turn
checks, notifications and WebUI endpoints.
Automatic fills retain their existing approved audit row. Browser and upload
adapters still revalidate the selected profile, destination and bytes before input.

The standalone library accepts explicit ports through `ExecutionHost::with_policy`.
Its unit fixture demonstrates a custom operation being blocked before dispatch,
then authorized through an independently supplied approval adapter. Returning a
confirmed exact receipt does not consume another approval or repeat input;
uncertain receipts remain blocked.

`Authority::verify_current` compares all scope fields. A component with changing
resources must supply fresh authoritative state after approval. This helper alone
is not a resource lease or a managed generation coordinator.

```sh
cargo test --locked -p kageha-policy -p kageha-runtime-service
```

## Owned persistence and remote decisions

`journal::Journal` owns its SQLite database and exclusive writer lock. Application
connections/tables are rejected. Approval requests, exact authority, immutable
decisions, one-time consumption and audit transitions are durable. The journal
consumes an approved decision before input; consumption is not evidence that the
input itself succeeded. Tool action receipts remain a separate responsibility.

A restart interrupts pending or approved-but-unconsumed decisions. It preserves
denied/consumed history, retains explicitly selected full mode and resets a
process-session mode. Late decisions and mismatched authority/revisions are rejected.
Cancellation is scoped to the session and turn. Session erasure purges request and
audit payloads and leaves a tombstone that blocks source reimport.

The WebUI adapter imports retained approval rows in ordered, restartable batches.
It preserves source rows and IDs. Legacy approved tokens become unusable audit
records, never permission to repeat input. Permission mode imports only once;
stale source settings cannot restore a later-revoked mode. New requests and
settings are written only to the policy journal. The compatibility database keeps
an import identity, not another live approval store.

The standalone runtime exposes authenticated approval list/status/decision APIs
using this journal. Configure `approval_tools` in its operator configuration to
require consent for an exposed tool. Agents cannot change this policy through a
run payload. A decision is accepted only for an active matching run; callers
cannot remotely consume a token or manufacture a request.

`tests/journal.rs` covers concurrent consumption, restart, stale authority,
cancellation/deadlines, migration checkpoints, malformed import rollback, deletion
and writer isolation. The runtime's remote approval fixture and WebUI migration
fixture verify the workflows in real Rust processes without Python.

Cross-host policy replacement, shared writer authority, grant transfer/rollback
and device enrollment remain release work. The retained source snapshot is not a
rollback destination for new decisions; rollback needs a managed reverse migration.

## Chat permission profiles

Explicit `PermissionProfile::{Ask, AutoReview, Full}` settings are chat-scoped and
separate from retained `PermissionMode` routine grants. Select with
`Journal::set_profile`; coordinate requests with `prepare_profile`, then `wait`.
Changing a profile revokes pending/unconsumed decisions, including reviews still
in flight. A manual approval under a chat profile is always one-use, even if a
legacy client requests a global grant. Full access requires host confirmation;
plan review still requires a human decision.

`ReviewPort` is replaceable. `kageha-models::reviewer::ModelReviewer` is an independent,
no-tools model invocation with a 15-second deadline, strict typed response and
exact digest validation. Invalid/unavailable reviews fall back to a pending human
request; denials do not become approvals. Review rationale is persisted separately
in the policy audit. Runtime-owned objective and steering provide user intent.

Both WebUI composers expose the three profiles. The Rust API reports tool/workspace
boundaries honestly: these profiles do **not** implement a Codex-equivalent OS
sandbox or grant operating-system permissions. See the focused
[Codex study](research/codex-permissions.md) for evidence and remaining parity.

## Managed requirements and review recovery

Set `KAGEHA_POLICY_REQUIREMENTS` to an operator-owned JSON file outside agent
writable roots. Requirements are validated at startup, not supplied by a model or
stored in editable chat settings. Invalid requirements fail startup.

```json
{
  "allowed_profiles": ["ask", "auto_review"],
  "forced_profile": "auto_review",
  "denied_actions": ["remote_exec"],
  "manual_actions": ["browser_upload"],
  "allow_review_override": false,
  "consecutive_denials": 3,
  "rolling_denials": 10,
  "require_process_sandbox": true
}
```

Omitted values use conservative defaults. Exact tool names identify denied or
manual actions. Denies apply to reads and cached receipts too; manual requirements
still apply under Full access. Legacy broad grants cannot bypass requirements.
The UI disables unavailable modes; backend enforcement remains authoritative.
Requirements changes require restarting the component with the new operator file.

Three consecutive denials, or ten denials in the last fifty reviews of a turn,
produce a typed terminal policy stop. The runtime scheduler stops new admission
and drains already-admitted operations; it never replays mutations. Unknown or
timed-out reviews are distinct from denials and reset the consecutive count.
Already-running reviews cannot approve after another review reaches the limit.

Authenticated operator APIs expose up to ten recent denials and permit one exact
reviewed retry. The marker is bound to the original action, session, binding,
generation, destination and input digest. It carries user intent into a fresh
independent review; it is not a dispatch token and cannot override managed denies.
Changed browser observations, files or targets need a new approval. Profile
changes revoke pending retry markers. There is no automatic click replay.

`delegate(parent, child)` establishes durable, immutable ownership. Specialist
workers inherit the owner's current effective mode, cannot escalate independently,
and see parent revocation immediately. Parent pending/denial views include workers.
Deleting an owner erases delegated decisions and leaves tombstones for every scope.

Review context contains user objective/steering plus bounded read-only evidence:
observed browser control role/name/frame and observation hash, or configured tool
binding identity and fingerprint. It contains no field values, file bytes or hidden
reasoning. Missing evidence falls back to human review instead of giving the
reviewer unrestricted host tools.
