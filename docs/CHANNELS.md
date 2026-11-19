# Messaging channels

Kageha uses one normalized channel pipeline for Telegram and WhatsApp. Each
adapter receives a platform message, deduplicates it, binds it to a durable
Kageha session, runs the agent, and delivers text or generated artifacts back
through the originating channel.

## Guided setup

**From the Web UI (recommended):** open Settings → Channels. Paste your
Telegram bot token or WhatsApp allowlist directly in the browser — no
terminal needed. Credentials are stored encrypted on the server (the same
store used for Gmail/GitHub/Phone). WhatsApp's QR code renders right in the
panel once you save your allowed number, instead of in a terminal. Each
channel card shows connection status and a "Test connection" action where
supported.

**From the CLI:** configure channels once with the wizard; credentials and
allowlists are saved to the project `.env` (which is gitignored):

```bash
kageha channels setup
# or include channel setup in the main first-run wizard:
kageha setup --channels
```

Either path works and they stay in sync — the Web UI checks the encrypted
store first and falls back to `.env`, so you can mix CLI setup for one
channel and the Settings panel for another. After configuring either way,
use `kageha webui` or `kageha chat`; configured channels start automatically.
There is no need to export channel variables in every shell.

## Telegram

Create a bot with `@BotFather`, then configure:

```bash
export TELEGRAM_BOT_TOKEN="..."
export TELEGRAM_ALLOWED_USERS="123456789"
kageha channels run --telegram
```

If `TELEGRAM_BOT_TOKEN` is missing and the command is run from a terminal,
Kageha explains the `@BotFather` setup and securely prompts for the token. The
token is used only for that process and is not written to disk. Non-interactive
shells should provide `TELEGRAM_BOT_TOKEN` and `TELEGRAM_ALLOWED_USERS`
explicitly.

The adapter uses long polling, so local development does not require a public
HTTPS endpoint. `TELEGRAM_ALLOWED_USERS` is deny-by-default; set
`TELEGRAM_ALLOW_ALL_USERS=1` only for a controlled test bot. Telegram messages,
photos, documents, video, audio, and voice notes are accepted. Generated
artifacts are sent as Telegram media or documents.

## WhatsApp QR (experimental)

The QR adapter links a WhatsApp account as a companion device through the
WhatsApp Web protocol. It is intended for local experiments and personal
accounts, not high-volume or production business messaging.

Install the sidecar dependencies:

```bash
cd integrations/whatsapp-qr
npm install
cd ../..
export WHATSAPP_QR_ENABLED=1
export WHATSAPP_QR_ALLOWED_USERS="15551234567"
kageha channels run --whatsapp-qr
```

The linked WhatsApp account can also message its own chat. The bridge
normalizes device-qualified WhatsApp IDs to the phone number before applying
the allowlist. For normal private-chat testing, use a second WhatsApp account
and allow that sender's international number without `+`, spaces, or dashes.

If `WHATSAPP_QR_ALLOWED_USERS` is omitted in an interactive terminal, Kageha
prompts for it. Use the WhatsApp sender's international phone number without
the leading `+` or spaces. Access from other numbers is ignored by design.

Scan the QR shown in the terminal using WhatsApp → Linked devices. Credentials
are persisted under `~/.kageha/platforms/whatsapp/session` by default. Keep this
directory private and back it up only through an encrypted mechanism.

If the command appears idle, wait for the QR banner; the bridge may take a few
seconds to connect. If it exits with an `asyncio` stream or line-length error,
update to the current sidecar and restart the command. The bridge suppresses
Baileys diagnostic logs and renders the QR on the terminal separately from its
JSON event stream.

The QR path uses an unofficial WhatsApp Web integration and may disconnect or
be restricted by WhatsApp. Do not use it for bulk messaging, unsolicited
messages, or accounts whose loss would be costly. For business or production
use, implement the official WhatsApp Cloud API adapter instead.

### Groups and delivery recovery

In a group, mention the linked account, reply to an agent message, or start
with `/agent`. Replies return to that group with separate conversation
history. `WHATSAPP_QR_GROUP_MODE=all` enables every group message; `off`
disables group responses. Sender allowlists still apply.

Replies and reminders persist before sending and retry after reconnects.
Retries reuse the same WhatsApp message ID and saved send receipt, including
across listener restarts. Heartbeats detect stalled connections. Encryption
failures use protocol retries without clearing all encryption sessions.

## Phone calls (Twilio)

The phone channel gives the agent a real phone line: it answers inbound calls
and, with the `telephony` tool pack, places outbound calls on your behalf. It
reuses the same durable runtime, memory, skills, and approval gates as every
other channel — a call is just another normalized turn.

Unlike Telegram/WhatsApp (which poll), phone is **webhook + websocket driven**,
so it runs inside `kageha webui` / `kageha server` and needs a public URL that
Twilio can reach.

### Setup

```bash
kageha channels setup     # enable phone; enter Twilio creds, number, allowlist, URL
```

This writes to `.env`:

- `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER` — your Twilio
  account and number (each user brings their own).
- `PHONE_ALLOWED_NUMBERS` — deny-by-default allowlist for **inbound** callers
  (comma-separated E.164). Unlisted callers are refused and never reach the
  agent. `PHONE_ALLOW_ALL_CALLERS=1` only for testing.
- `PUBLIC_BASE_URL` — the public **HTTPS + wss** base URL Twilio connects to.

Enable the outbound tools:

```bash
export KAGEHA_TOOL_PACKS=telephony   # or packs: [telephony] in tools.yaml
```

### Exposing a public URL

Twilio must reach your server for inbound calls and the live audio stream. The
media stream uses a websocket (`wss://`), so the URL must pass websockets.

```bash
# Laptop / behind NAT — run a tunnel, then set PUBLIC_BASE_URL to its https URL
ngrok http 8788
cloudflared tunnel --url http://localhost:8788

# Server with a public IP — open the port and use its https URL directly
```

Then point your Twilio number's **Voice webhook** at
`<PUBLIC_BASE_URL>/webhook/phone` (HTTP POST). Check readiness any time:

```bash
kageha channels run --phone   # prints an outbound/inbound readiness report
```

### Voice providers

Twilio media calls default to live speech with Gemini or OpenAI. Configure
`GEMINI_API_KEY` or `OPENAI_API_KEY`, and select `PHONE_LIVE_PROVIDER=gemini`
or `openai`. Both support streaming audio, interruptions, and delegation to
Kageha's tools. `PHONE_VOICE_MODE=turn` selects the older registry-backed
STT/agent/TTS loop; its providers are configured in `models.yaml`.

Browser live voice uses the same providers. Choose Gemini or OpenAI in the
voice overlay. See [the README](../README.md#live-voice-and-phone-calls) for
model and voice overrides.

### Outbound calls (agent calls on your behalf)

With the `telephony` pack on, ask the agent to call someone. It confirms the
number and objective, then `phone_call` places the call — **approval-gated**, so
you approve before it dials a real person. Poll `phone_call_status` and read
`phone_transcript` for the outcome. The `phone` skill guides this flow.

### Safety and limits

- Inbound is deny-by-default (`PHONE_ALLOWED_NUMBERS`).
- Outbound dialing is HITL-approved (`phone_call` is `risk_class=messaging`).
- `PHONE_MAX_CALL_SECONDS` (default 600) caps call length.
- Recording is off unless `PHONE_RECORD_CALLS=1` — confirm consent and local law.
- Outbound calls speak an AI-caller disclosure first (`PHONE_AI_DISCLOSURE`).
- Live calls support interruptions; `PHONE_VOICE_MODE=turn` retains the older turn-based loop.

## Keeping channels running

Channels are automatically started as a supervised background service when
`kageha chat`, `kageha webui`, or the long-lived app server starts and the
channel environment is configured. A one-shot `kageha run` does not start
channels. Set `KAGEHA_CHANNEL_AUTOSTART=0` to disable this behavior and run
`kageha channels run --whatsapp-qr` manually instead.

For supervised startup, configure the allowlist in the environment first so a
non-interactive background process does not have to prompt:

```bash
export WHATSAPP_QR_ENABLED=1
export WHATSAPP_QR_ALLOWED_USERS="15551234567"
kageha webui
```

For a reliable smoke test, send a message from a second WhatsApp account to the
linked account. Self-chat is also supported when WhatsApp forwards the event;
the adapter normalizes the linked account's device-qualified ID and guards
against outbound reply loops.

Inspect or stop a supervised listener with:

```bash
kageha channels status
kageha channels stop
```

If the listener was started directly with `kageha channels run`, stop it with
`Ctrl-C`; WebUI and chat detect that process and will not create a duplicate.

## Media and safety

- Inbound media is downloaded before the agent turn and referenced as a local
  input file.
- Outbound files are resolved only inside the completed Kageha session
  workspace.
- Channel identities are hashed in the durable queue; raw identities are not
  persisted there.
- Duplicate inbound events are ignored before starting an agent turn.
- Per-peer turns are serialized so two messages cannot mutate one session at
  the same time.
- Channel turns do not auto-approve risky tools. Approval-required work is
  reported back as blocked and should be continued from the Web UI or CLI.
- WhatsApp is deny-by-default: senders not listed in
  `WHATSAPP_QR_ALLOWED_USERS` are never passed to the agent and receive no
  agent reply. Keep `WHATSAPP_QR_ALLOW_ALL_USERS` unset except for isolated
  testing.

## Current limitations

- Telegram currently uses long polling; webhook mode is planned.
- WhatsApp QR is experimental and requires Node.js plus the sidecar install.
- Rich canvas interactions remain in the Web UI. Channels receive text,
  previews, and downloadable artifacts rather than the interactive canvas.

## Adding a new channel (Slack, Microsoft Teams, ...)

The Settings panel and its API routes are generic — adding a channel means
writing a descriptor, not a new panel or new routes:

1. **Runtime adapter** (`src/kageha/channels/<name>.py`): a small class with
   a `channel` id and a `run()`/event handler that normalizes provider
   payloads into `ChannelMessage` and calls `runtime.process(message)`, then
   sends the returned `ChannelReply` back through the provider's API. Model
   it on `telegram.py` (polling) or `phone.py` (webhook-driven).
2. **Config module** (`src/kageha/channels/<name>_config.py`): `status()`,
   `save_configuration()`, `disconnect()`, and optionally `check()`, backed
   by the encrypted `kageha.integrations.storage` store with `.env` as a
   fallback. Model it on `telegram_config.py` or `whatsapp_qr_config.py`.
3. **Register it**: call `registry.register(ChannelDescriptor(...))` from a
   `register()` function in the config module, and add that module to
   `registry.ensure_builtins_registered()`.
4. **Settings UI**: nothing to do — `ChannelsSettings.tsx` renders every
   registered channel's card and config form from its `ChannelDescriptor`
   fields automatically. Only build custom UI (like Phone's live-call
   transcript or WhatsApp's QR image) if the channel needs something beyond
   a status pill and a save/disconnect form.
5. **Supervisor/CLI** (only if the channel needs a long-lived polling
   process rather than a webhook): add it to the `channels run` command in
   `cli.py` and to `_any_channel_configured()` in
   `src/kageha/runtime/supervisor.py` so `kageha webui`/`kageha chat`
   autostart it.
