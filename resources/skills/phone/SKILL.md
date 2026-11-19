---
name: phone
description: Place and receive real phone calls (Twilio). Use to call someone on the user's behalf — confirm an appointment, ask a question, relay a message — then report what was said. Inbound calls are answered automatically.
triggers:
  - call
  - phone
  - dial
  - ring
  - voicemail
  - call them
  - call him
  - call her
  - give them a call
  - phone call
  - make a call
allowed-tools: phone_call phone_call_status phone_transcript ask_human
---

# phone

Real voice calls over the phone network. Outbound: you dial a number and hold a
spoken conversation to accomplish an objective. Inbound: callers reach the
assistant automatically (handled by the phone channel, no tool needed).

## Before you dial (required)

Placing a call reaches a real person and may cost money, so **always confirm
first**:

1. Confirm the exact **number** in E.164 form (e.g. `+14155552671`) and the
   **objective** with the user. If either is unclear, use `ask_human` — do not
   guess a number.
2. Then call `phone_call(to_number=…, objective=…, greeting=…)`. It is
   approval-gated: the user must approve the dial. If denied, do not retry;
   follow the user's feedback.
3. `objective` should be a crisp instruction the on-call assistant pursues
   (e.g. "Confirm John's dental appointment on Tuesday at 3pm; if that time is
   gone, ask for the next available slot"). `greeting` is the optional first
   spoken line after the automated-caller disclosure.

## After dialing

- Poll `phone_call_status(call_ref)` for progress: `queued` → `ringing` →
  `in-progress` → `completed` (or `busy` / `no-answer` / `failed`).
- Once the call ends, read `phone_transcript(call_ref)` and summarize the
  outcome for the user. Quote the key result (e.g. the confirmed time), not the
  whole transcript.
- If the call failed or hit voicemail, tell the user and offer to retry or send
  a text instead — do not silently redial.

## Keep spoken replies short

On a live call, answers are heard, not read. Speak in short, plain sentences.
Avoid lists, markdown, and URLs (say "I'll text you the link" instead).

## Setup (tell the user if calling is not configured)

Phone calling needs Twilio credentials and a publicly reachable server:

- `kageha channels setup` → enable phone → enter `TWILIO_ACCOUNT_SID`,
  `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`, allowed inbound callers, and
  `PUBLIC_BASE_URL`.
- Enable the tools with `KAGEHA_TOOL_PACKS=telephony` (or `packs: [telephony]`
  in `tools.yaml`).
- **Inbound + live audio require a public HTTPS+wss URL** Twilio can reach. On a
  laptop use a tunnel (`ngrok http 8788` or `cloudflared tunnel`); on a server,
  open the port. Point the Twilio number's Voice webhook at
  `<PUBLIC_BASE_URL>/webhook/phone`.
- Voice quality follows the configured TTS/STT providers (Gemini, Cloudflare,
  OpenAI, or ElevenLabs when enabled) — no vendor is hardcoded.

## Safety

Inbound is deny-by-default: only numbers in `PHONE_ALLOWED_NUMBERS` reach the
agent. Calls have a duration ceiling (`PHONE_MAX_CALL_SECONDS`) and recording is
off unless `PHONE_RECORD_CALLS=1`. Never place a call the user did not ask for.
