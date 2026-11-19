---
name: computer_use
description: Native macOS control through Kageha Computer, the Rust executor.
triggers:
  - computer_use
  - macos
  - desktop app
  - native app
  - calculator
  - textedit
  - finder
  - kageha computer
  - ax tree
  - phone call
  - dial
  - facetime
  - video call
  - voice call
  - iphone mirroring
  - whatsapp call
  - voip
allowed-tools: computer_doctor computer_launch computer_list_apps computer_list_windows computer_observe computer_click computer_set_value computer_type computer_key computer_focus computer_select_text computer_scroll computer_drag computer_secondary_action computer_batch computer_benchmark ask_human
---

# computer_use

This skill owns native macOS UI through Kageha Computer. It is not `/browser`
or `/machine`. Do not call `browser_*` for a native app task or ask for a Linux box.
Use the registered `computer_*` tools; do not operate native apps with shell,
AppleScript, PyAutoGUI, or unregistered legacy tools.

## Observe and act

1. Call `computer_observe(target=app)` to bind the exact app/window. A screenshot
   is included by default (JPEG; `image_format="png"` requests lossless pixels).
   Use `screenshots=false` only for a deliberate AX-only
   lookup. Missing Screen Recording permission is reported; use `computer_doctor`
   to identify the required permission. Stop at a missing grant and give the
   returned helper path and exact missing permission to the human. Probes and
   launching the helper do not request Screen Recording; never promise a prompt.
   Do not automate permission settings, query TCC, or use shell to bypass a denial.
2. Pass its `target`, `observation_id` and a new caller-generated `request_id`
   with each input. Prefer observed refs (`eN`) over coordinates. Pixel actions
   require a screenshot of that window and unchanged window geometry.
3. Keys, value replacement and selection return a fresh AX-only
   `verification_observation` in the same call. Inspect it and use its new
   `observation_id` for further input; avoid an extra identical observe call.
   `verified=false` on a dispatched key is an acknowledged dispatch, not proof
   of failure or success. Never replay it just
   because its resulting UI effect is not yet verified. App-name observations
   follow that app's focused dialog/window; exact `macos:` targets stay pinned.
   After other mutations, observe again before deciding the next action. Old refs are
   stale. An uncertain input must be reconciled from fresh state, never replayed.
4. Group deterministic actions with known, stable targets in
   `computer_batch(verify=true)`. Click accepts `ref` or window-local `x,y`;
   `set_value` requires `ref` and `value`. Prefer values for native editable
   fields. The executor preflights the batch, stops on failure and returns partial
   effects. A key ends the batch so later refs cannot run against a changed UI.
   Use observed secondary `AXOpen` actions for file entries without `AXPress`.
   Do not group actions requiring unknown intermediate state.
5. Inspect `verification_observation` before further input. `verification=observed`
   attaches fresh evidence; it does not prove the user's goal. Quote returned
   `readings` when reporting success. Once the goal is visibly present, stop.

A duplicate request_id retrieves the prior outcome. It does not execute again.
Do not screenshot-loop: each observation must resolve a concrete state question.
If AX is empty/degraded, use the attached window image for a targeted action,
then observe again. Never invent refs, coordinates, or an unseen successful result.

## Native behavior and boundaries

- Rust owns capture, AX handles, input and native batches. Python connects those
  operations to Kageha's approvals and model context. Clef/Jev guidance appears
  only when configured and does not override approvals or certify outcomes.
- Native text operations use AX values/selected text. Web-content AX writes are
  rejected; do not treat a dispatched event as verified renderer input.
- `computer_select_text` requires an observed ref. Modes are `all`, `text`,
  `cursor_before` and `cursor_after`; prefix/suffix disambiguate a text match.
  Its `verification_observation` supplies the new token for subsequent typing.
  For replacing an entire editable value, use `computer_set_value` directly;
  selection plus typing adds unnecessary steps.
- Keys require the exact focused window. If returned `focus` shows another
  window, use `computer_focus` once with the observed target and a new request ID.
  This explicitly brings that app forward and returns fresh AX state; inspect
  its focus before further input. A blocked modal sheet must be observed as its
  own target. If focus still fails, report the blocker. Never replay failed keys,
  restart apps, or use shell as a focus fallback.
- The Apple menu is application chrome, not an overlay. Never click it merely
  to dismiss a dialog. Use Escape only when an actual menu/sheet is observed.
- Hard-blocked apps include Terminal, Kageha and Kageha Computer. Password/MFA,
  permission, CAPTCHA and unapproved confirmation prompts require human handling.
- Launch by installed bundle ID when an app is not running. Resolve ambiguity
  before input; do not select another window merely because the target failed.

## Live WebUI takeover

The Live panel follows the observed Mac window. Click **You** before user input;
switch back to **Agent** to resume agent work. Preview input is scaled to that
window. Preview capture is pixels-only and preserves the agent's AX snapshot.
The executor serializes native requests. Coordinate changes, takeover and helper
reconnects require a fresh observation. Never issue dependent inputs concurrently.

## Calling tasks

For a requested call, resolve the contact/number, audio/video choice and named app.
Use `computer_launch`, observe the app, choose visible controls and verify ringing,
connecting or in-call controls for the intended recipient. A launch or dispatch
acknowledgement is not a placed call. If a device is locked, ask the human to
unlock it. Do not type credentials or invent a number. Report the concrete blocker
when the requested app cannot complete the call.
