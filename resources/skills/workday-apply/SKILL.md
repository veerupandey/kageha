---
name: workday-apply
description: Complete an authorized Workday application through the managed Kageha browser. Handle sign-in, React forms, uploads, review, and submission evidence using the current session tools.
metadata:
  version: "1.5.0"
  author: kageha
---

# Workday applications in the managed browser

Use this procedure only for an application the user authorized. The job-apply skill owns screening, tailoring, and the answer pack; this skill covers form mechanics.

## Browser and control

Inspect browser_tabs, durable pending user questions, existing application receipts, and the current browser_snapshot first. Resume an existing managed draft tab before opening anything new. Use browser_open only when the exact application is not already open. Keep every action on that managed tab so the right panel follows the work.

Use the currently advertised managed browser tools and inspect their schemas before supplying arguments. Read page state with browser_snapshot and browser_inspect for targeted selectors. Pass compact=false only if that tool's current schema advertises it. Prefer browser_click, browser_fill, browser_select, browser_check, browser_press, browser_interact, browser_batch, and browser_upload when available.

The Rust browser_script is a synchronous JavaScript workflow with the browser facade: browser.snapshot(), browser.batch([...]), browser.tabs(), browser.select(tabHandle), browser.navigate(url), browser.wait(ms), and approved artifact/upload operations. It has no document, window, location, page, cdp, Playwright, Python, filesystem, or network globals. To read state, use `const state = browser.snapshot(); return state;`. Do not put DOM evaluation or asynchronous Python in this tool. Use browser_inspect instead of inventing a browser_evaluate API. Do not connect a shell script to a debug port or create a separate CDP session. Old scripts in this package are historical examples, not runnable browser transports.

If tools report user control, respect that ownership. Wait for the user to return control or use the managed handoff/approval flow. Do not retry through another transport. A private browser also has ownership rules. Use request_secure_login for the user's sign-in handoff; never bypass an access pause.

## Procedure

1. Confirm the exact employer, requisition, and application URL from the authoritative posting. Do not guess tenant paths or identify a job by title alone.
2. Inspect whether the page requires Sign In or Create Account. Prefer recovering an existing authorized account. Check authorized Gmail for activation, reset or sign-in messages before asking the user. Validate the exact employer tenant and email destination; do not follow unrelated reset messages. If the runtime advertises managed credential support, use `browser_credential(account)` and `browser_fill_credential`, or the version-3 script operations `browser.credential(account)` and `browser.fillCredential(target, reference)`. These generate and reuse a stored opaque credential; never generate a password in model text, put it in ordinary fill, or save it in artifacts. Use the same reference for confirmation and later sign-in. Verify the resulting page after the account button; filling a password is not sign-in evidence. Create an account only when authorized and recovery confirms no existing account. Check the page before waiting for a code; some sites do not send one. Keep codes and reset tokens out of narration and saved artifacts and avoid repeated resends. Request a human handoff only for an actual missing credential capability, inaccessible second factor, CAPTCHA or missing authorization.
3. Inspect each form step, create a compact question-to-control map, fill independent stable controls, and read back their committed values. Use native managed fill/press/check/select operations for React fields. When a click changes a menu or page, inspect fresh state before reusing references. Do not inject DOM setters. For comboboxes, click the visible option and verify selection. If several buttons say only “Select One,” inspect their exact IDs and associated labels or ancestor fieldset context. Match each ID to a fresh observed reference; never choose by index or guess which question an open menu belongs to. Read back that same question's selected value before saving. Use the date picker if typed dates do not commit.
4. Upload the actual tailored resume with browser_upload. For work history or education, count existing entries before adding; retries can duplicate records. Do not add a duplicate to recover from an uncertain click.
5. After Save and Continue, verify that the next step appeared and inspect validation messages. Repair observed errors rather than repeating the same batch blindly.
6. At Review, read back the complete application against the answer pack. Submit only within the user's existing authorization, asking only if a material new commitment or required fact is outside the existing authorization.
7. Verify an actual confirmation or candidate-home entry for the exact requisition. Use native linked receipt confirmation for formal evidence; a screenshot alone is not submission proof. Update the tracker with the actual observed status. A login screen, click, or uncertain redirect is not submission evidence.

A genuine authentication or missing-answer blocker is a valid handoff. State exactly what is needed and preserve the task for continuation.

## Segmented dates and styled checkboxes

A date's accessible Month/Year input may be tiny while a separate visible segment
displays its value. Direct Year focus can redirect to Month. Complete the date
before clicking elsewhere: fill Month, observe its actual focus, press ArrowRight
to the next segment, observe Year focus, then type the year with normal key
presses. Verify the complete visible date and cleared validation. Try Tab once
if ArrowRight does not move focus. Do not force-click tiny hidden input geometry,
inject values, or leave a partial date while editing other controls: validation
on blur can reflow the page and interrupt the next click.

Use browser_check for a native checkbox with a visible associated label; the Rust
runtime hit-tests that label and verifies the native checked state. If the page
changes during the click, observe and reconcile the pending receipt. A retired
hold whose current state differs is not success. A fresh corrective check may
then set the user's requested state; never blindly replay the earlier click.

## Upload recovery

Inspect existing attachment filenames before uploading. Use a unique observed
CSS file-input selector for browser_upload, not an accessibility e-reference.
Some widgets clear FileList immediately after accepting bytes. If a receipt stays
returned while the exact filename is visibly attached, use browser_receipts and
browser_reconcile rather than uploading again. For an older receipt that omitted
the filename metadata, supply attachment_path for the exact pinned chat-owned
original file. This retires the legacy hold on visible attachment evidence; it
does not prove server bytes, a saved application step, or final submission.

After repairing a widget, continue the remaining authorized draft work. Known
resume facts and an intermediate successful field are not missing-answer
blockers. Before the final report, take fresh state and report the actual current
step, rather than repeating an obsolete account or validation failure.

## Final submission and completion routes

Use the advertised deferred-confirmation submission path when success wording is not known in advance. Preserve the original final-click request ID and never dispatch a second Submit after a returned or uncertain action. An empty pending list is not empty receipt history: recover the original request by outcome or request ID. Reconcile once using the actual site result; do not guess evidence references.

Workday's `jobTasks/completed/application` route omits the requisition. Native confirmation may bridge it only from the original protected job receipt with exact tenant, same tab/document/profile/session continuity, and a visible Application Submitted dialog with affirmative confirmation. Similar URL, job-description text, a candidate-home page, or another document cannot substitute for that provenance. Preserve the exact requisition including numeric suffixes. If the adapter cannot establish provenance, report site confirmation and its formal evidence limitation once; do not resubmit or navigate to reconstruct proof.

## Bounded recovery and sourced answers

Refresh affected controls once after a stale reference or React remount. Use the existing mapping for unchanged fields instead of repeatedly reading full snapshots or spilled files. Honor recovery and unchanged-read exhaustion; a new observation ID is not a new form state. After a genuine user answer or verified field change, continue from the current draft.

Scope custom dropdown options to the associated question/menu, including truthful offered Other/Not Listed source options. Verify a styled checkbox by its checked state rather than hidden-input visibility. Keep date segments together until the full date commits. A pending submit safeguard never authorizes input through a different transport.

Keep exact HR questions and explicit answers in private session context. Separate conditional follow-ups, citizenship, existing authorization, future status needs, and relocation. Do not infer answers from earlier related questions or resume omissions. Verify sign-in on the exact tenant; a vault credential reference or another employer's login is not proof. An explicitly authorized password reset is bounded to that account and attempt.
