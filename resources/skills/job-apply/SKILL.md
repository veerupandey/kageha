---
name: job-apply
description: Complete authorized job applications, triage job alerts, resume existing drafts, and track exact requisitions without duplicate submissions. Use for LinkedIn and ATS applications, including Greenhouse, Ashby, Workday, Lever, and Jobvite. Collect missing facts with scoped user questions, preserve browser receipts, and distinguish site confirmation from formal evidence limitations.
metadata:
  version: "2.0.1"
  author: kageha
  purpose: duplicate-safe applications with bounded recovery and factual answers
---

# Job applications

Use the user's current authorization and preferences. A request to apply authorizes the necessary form work and submission within that scope; do not add a redundant batch or per-submit approval. Ask only for missing facts, a material new commitment, an account change outside existing authorization, or an actual access handoff. Continue independent authorized applications while one waits.

Read `references/skill-learnings.md` when diagnosing a recurring failure. For Workday form mechanics, load `workday-apply`. Historical Python/browser helpers in this package are not the managed browser transport; their company/title duplicate matching is insufficient for submission decisions.

## 1. Recover state before acting

Read the session tracker, user answers, original application receipts, and current managed tabs. Resume the existing draft at its observed step. A summary, stale error, credential reference, or a pending-receipts list is not the current page state. An empty pending list does not mean an old receipt was deleted: look up its original request ID or outcome history.

Keep these session-private artifacts, outside the reusable skill package:

- Candidate facts and answers: source, date, exact question, destination/employer, jurisdiction, and scope of reuse. Use `references/profile.template.md` as a structure; populate only facts supported by the user's material or explicit answers.
- Application tracker: employer, ATS tenant/provider, exact requisition, observed URL aliases, role/location, draft tab, current step, status, original submit request ID, confirmation reference, and unresolved blocker.
- Tailored resume and the actual posting used to tailor it. Keep factual employment, skills, dates, and achievements accurate.

Do not copy personal answers, addresses, credentials, tokens, reset links, or resume contents into this skill, its learnings, or shared reports. Keep credential material in the managed vault and use opaque references.

## 2. Screen and deduplicate

When asked to check alerts, read the authorized source and record a sweep cursor. Follow the actual posting to verify employer, location, work arrangement, requirements, salary currency, and application destination. Unposted salary is unknown; do not infer a salary floor or candidate preference from seniority. Apply the user's stated filters, and clarify material mismatches.

Identify a job by employer/tenant plus exact provider requisition, using observed aliases to join LinkedIn alerts to the same ATS job. Similar titles can be different jobs; multiple city alerts can be the same requisition. Do not strip Workday suffixes or infer that two similar IDs are equivalent. The tracker is a useful index; native receipts and the actual site decide whether a submit already happened.

If status is submitted, site-confirmed, submission-pending, or submission-uncertain, do not submit again. Inspect the existing application and reconcile its original receipt. Never create a second account or draft to work around an unresolved submit.

## 3. Gather answers without inventing them

Extract ordinary resume facts before asking. Persist an explicit user answer immediately and reuse it when the same question and authorized scope apply. Recheck outdated facts or a materially different question. Do not treat an answer as universal or permanent.

Citizenship, current work authorization, eligibility to obtain a status, future sponsorship, relocation, and onsite willingness are separate facts. A follow-up about a narrower class of conduct or employment does not inherit the preceding Yes/No answer. Do not infer legal history, government employment, conflicts, restrictive agreements, salary expectations, or personal declarations from resume omissions.

Ask the user for required unknowns with the exact wording and necessary context. Bundle independent questions where useful. Optional demographic answers are voluntary: select a decline option only according to the user's preference; never invent a demographic fallback. Ask about an unexpected required declaration, especially one concerning personal work or external assistance. Preserve the user's response and its context.

A durable pending `ask_human` means waiting for the user, not a browser failure. Check pending questions before retrying or diagnosing a stalled model. Resume the preserved draft after the answer; do not restart the application.

## 4. Complete each form step

Use the currently advertised managed browser tools and schemas. Read with `browser_snapshot` or a targeted `browser_inspect`; mutate with native fill, credential-fill, check, select, press, click, and upload tools. Do not invent APIs, inject DOM setters, or switch to a separate browser/CDP/shell transport to bypass a hold.

Make one compact field map per observed step: question/label, exact ID or reference, control type, required state, intended sourced answer. Use it to fill the step. After navigation, menu changes, React remounts, or a stale reference, inspect the affected region and refresh references. Batch only independent controls whose references remain valid; do not force an entire reactive form into one stale batch.

- Numeric fields: use native fill with the field's actual numeric value and observe its accepted value and validation. Do not switch to unsupported typing or scripts.
- Dropdowns: open the correct observed control, inspect its visible options, select the matching option, and read back that question. Scope options to the menu/fieldset; a footer link named LinkedIn is not an application-source option. Use Other/Not Listed only when visibly offered and truthful.
- Styled checkboxes: use native check with the observed associated label and inspect the checked state. Visibility of a hidden input is not its checked-state proof.
- Uploads: inspect existing attachments first. A returned/uncertain upload may already have attached the file; reconcile against the exact filename and original receipt before uploading again. Attachment evidence is not final submission evidence.
- Repeated sections: count existing employment/education entries before adding. Never replay an uncertain Add click.

A click receipt without an asserted postcondition acknowledges input delivery. Verify the actual panel, URL, visible fields, or message before claiming a login toggle or Continue action advanced the form. If state stays unchanged, permit one targeted repair and stop that route on the same result. Do not alternate login/signup toggles or fetch site bundles to evade an unproductive browser route.

After Save/Continue, verify the next step or read the actual validation errors. Repair the named fields and verify their committed values. Checkpoint every completed stage so interruption resumes at the next outstanding step.

## 5. Review, submit once, and verify

Read back the complete application against sourced answers and the tailored resume. Resolve required unknowns and material inconsistencies before the final action. Define the native application outcome for this exact requisition using the advertised submission proof schema.

Use the managed submission path. When supported, deferred confirmation allows Submit without guessing success wording: the original action remains fenced until independent site confirmation. Do not invent a pre-known confirmation message to satisfy a tool requirement.

After a returned, timed-out, or uncertain final click:

1. Preserve the original request ID and exact application identity. Do not click Submit again.
2. Read current state on the original tab. Use receipt history/outcome lookup even if the pending list is empty.
3. Reconcile the original request with fresh independent confirmation once. Use the returned verification reference for outcome evidence; an arbitrary click, screenshot filename, or guessed request ID is not interchangeable proof.
4. Report site-confirmed, formally verified, validation-blocked, or uncertain accurately. If the provider/provenance is unsupported, preserve the observed result and state that formal evidence is unavailable once. Do not resubmit to manufacture proof or weaken the required outcome.

A Workday `jobTasks/completed/application` URL omits the requisition. Its confirmation can be linked only through the original protected receipt with same tenant, tab, document, profile, and session continuity plus a genuine visible confirmation. A matching generic URL or success text inside a job description is insufficient. Never navigate merely to recreate a completion page.

## 6. Recover within bounds

Classify the failure before changing anything:

| Signal | Next action |
|---|---|
| Stale observation / remounted field | Inspect affected controls once; use fresh references for the same intended field. |
| Validation error | Repair the actual named field; inspect the committed answer before continuing. |
| Pending/uncertain action or upload | Read and reconcile its original receipt; do not replay it. |
| Unchanged field map or repeated evidence rejection | Use existing mapping/proof. Stop remapping and guessing references; change strategy or record the limitation. |
| Vault locked / user control / MFA / CAPTCHA | Use the supported unlock/handoff and wait for the real result. |
| Unsupported confirmation provenance | Preserve site result and original receipt; report the formal limitation without another submit. |
| Missing required user fact | Ask the exact question; continue independent authorized work. |
| Journal/storage error | Stop mutations, preserve receipts, and report the harness failure. Do not clear history to make room. |

Permit one targeted repair after a failure when evidence supports it. A second equivalent failure with unchanged state exhausts that route. Fresh observation IDs, another selector spelling, another screenshot, or rereading a spilled output file are not a changed strategy. Respect `recovery_exhausted` and `observation_loop_exhausted`; do not route around them with scripts or unrelated diagnostic tools. A genuinely changed form state or a new user answer can justify fresh work.

For account recovery, scope the account to the exact employer tenant. A vault reference proves only storage, and sign-in at one tenant proves nothing about another. Verify the actual signed-in page. Password resets require explicit authorization for that account; after a successful reset, verify sign-in before considering any further recovery. Avoid repeated resends and unbounded mailbox polling.

Checkpoint a blocked application and continue independent authorized work. Final reporting should distinguish submitted/verified, site-confirmed with a proof limitation, draft/blocked, closed, and excluded, with one precise next action for each outstanding item. Do not report completed work as failed merely because a formal evidence adapter could not prove it.

## Maintain learnings

Promote a lesson only after identifying its cause and checking the remedy. Harness invariants belong in code with regression tests; reusable workflow decisions belong here or in `workday-apply`; candidate answers and employer-specific application state remain private. Update existing guidance instead of appending conflicting hard rules. Record symptoms, safe recovery, evidence, and limits in the concise learnings reference. Do not turn every site-specific failure into a universal rule.
