# Kageha Personal

A private, single-owner assistant running as a separate Kageha instance. Texts, calls, the paired Mac and the local CLI share a durable task queue. Each new request starts an ordinary Rust chat turn, with its own browser session and a Linux machine available in the Computer panel. Kageha handles tools, permissions, memory and lessons in the personal instance. The companion records incoming messages, delivery receipts, deadlines and routing in SQLite.

## Use your instance

Open [Kageha Personal](http://127.0.0.1:8889). For the initial login, copy the private token with `pbcopy < ~/.kageha-personal/web.token`. Select a chat and open **Computer** to view its Linux desktop or **Browser** for its browser. The host must be awake and Docker must be running to use the Linux machine.

From the repository root:

```sh
node integrations/personal/personal.mjs ask "Find three repair shops open tomorrow"
node integrations/personal/personal.mjs status
node integrations/personal/personal.mjs ask "Only after four #1"
node integrations/personal/personal.mjs messages
```

WhatsApp, SMS, and iMessage handle greetings and quick answers in one message. For longer requests, a short progress update can tell you what Kageha is doing. Ask “Any updates?” for the latest status, or answer a single pending question directly. Casual replies can include an emoji. The linked-device adapter sends typing indicators while preparing a reply and clears them when its text queue drains. When a request takes longer than eight seconds, Kageha sends a short acknowledgement. Regular WhatsApp follow-ups stay in the same chat. Send `/new` to start fresh or `status` for a progress update. Send `status #1`, `cancel #1`, or a follow-up mentioning that number. Use `send result via WhatsApp #1` to change that task’s destination after pairing WhatsApp. Independent requests can run while another task waits for a result or a decision. The CLI supports `KAGEHA_PERSONAL_CONFIG` for another profile.

## Install on another host

Clone the Rust repository with your own GitHub account and run the setup commands from its root. Setup creates a new private directory with fresh credentials for your instance.

```sh
git clone https://github.com/veerupandey/kageha.git
cd kageha_rust
```

Use Node.js 24, a current Kageha checkout, Docker with an accessible engine, and Chrome or Chromium. On Linux the installer uses a systemd user manager. Enable lingering for the service account if it needs to run across logouts. On macOS services run in the logged-in user's launchd domain.

```sh
cargo build --locked --release -p kageha-loop -p kageha-browser -p kageha-workspace \
  --bin kageha-loop --bin kageha-browser --bin kageha-shell-service
npm --prefix frontend ci
npm --prefix frontend run build
npm --prefix integrations/personal ci
docker build -f docker/desktop/Dockerfile -t kageha-desktop:machine-v3 .
node integrations/personal/setup.mjs "$HOME/.kageha-personal" --local
```

Set `chromium` to the installed browser executable if it differs from the system default. In the private `personal.json`, set `model` and `modelUrl` for an existing local model, or configure the personal instance’s provider in Kageha settings. For LM Studio you can also supply `localModel` with the absolute `cli` path, model `key`, and `contextLength` so the backend can load a dedicated alias when it starts. The tested host uses a 32,768-token context.

```sh
KAGEHA_RELEASE_DIR="$PWD/target/release" node integrations/personal/service.mjs install "$HOME/.kageha-personal"
```

Keep the checkout, frontend build and Node dependencies available at their installed paths. The browser, Rust backend and messaging gateway each have their own supervisor. State, credentials and workspace are in the private instance directory. Point `KAGEHA_RELEASE_DIR` at your actual build output when using a custom Cargo target. Re-running the installer stages the binaries. Once the personal instance is idle, restart the three `org.kageha.personal.{browser,backend,gateway}` services with launchctl or systemctl to activate changes.

## Pair messaging and voice

To use a stronger model for messages, set `"channelModels": ["cf-deepseek-v4.1-flash", "cmd-deepseek-v4-flash"]` in the private `personal.json`. Configure and test the two providers in the personal backend’s model settings, then restart the personal gateway when idle. The first model handles personal gateway turns. The Rust executor uses the second as a fallback. Omitting the list inherits the backend model. The web UI and agents routed to a shared backend use their own model settings. With a hosted model, channel conversation context and file content used by the model are processed by its provider.

The local installation can run before pairing any external accounts. To connect a carrier, configure a dedicated Twilio number and a public HTTPS reverse proxy. Forward only `/carrier/*` and, if using a remote Mac, `/bridge/*` to the loopback gateway at port 8891. Keep the Rust backend, `/owner/*` and `/internal/*` private. Signatures use the public origin, path and query exactly as received.

- Set `publicUrl` (HTTPS origin), `sid`, `auth`, `ownerPhone`, `number`, and `voicePin` (6–12 digits) in the owner-only configuration. Use international phone numbers starting with `+`. Restart the personal gateway after configuration.
- Point the incoming SMS and WhatsApp message webhook to `POST /carrier/message`. Set `wa` to the registered WhatsApp sender, including its `whatsapp:` prefix. To deliver outside an open WhatsApp conversation, configure `template` with an approved content SID that accepts the message in variable 1. Otherwise pending messages wait for the next inbound WhatsApp message.
- Point the incoming phone call webhook to `POST /carrier/voice`. Calls from the paired owner number require the PIN. Speak a request or ask for status. The call can end while the task continues and the assistant delivers the result by SMS. Audio notes need an STT provider configured in the personal instance.
- To pair iMessage, run the bridge on a signed-in Mac with Messages and the `imsg` command installed. Give that process the macOS permissions required by Messages. In the private `bridge.json`, use the HTTPS origin for a remote server or `http://127.0.0.1:8891` on the same Mac. Set the direct `chatId`, exact sender `owner`, bridge token and an absolute local database path. Set the matching `macOwner` in `personal.json`. Run `node integrations/personal/mac-bridge.mjs /path/to/bridge.json` in that login session. The bridge receives new owner messages after pairing. It can reveal an explicitly approved absolute file path in Finder.
- On a new installation, omit `--local` from setup to collect the origin and paired addresses interactively. Store tokens and the voice PIN in the private profile.

## Approvals and follow-ups

In linked WhatsApp, answer the latest browser or file approval with `yes` or `no`. You can also swipe-reply to a specific approval. If the conversation has moved on, Kageha repeats the action before accepting a new answer.

For a vendor call, the assistant records the exact phone number and script. Reply to the proposed action with `approve #1 P-…` or `deny #1 P-…`, using the exact IDs received. A completed call includes the carrier status and any spoken answer. Reusable call rules in private configuration match both the full number and the full script. For eligible native Kageha approvals, `always approve #1 APPROVAL_ID` saves the exact action fingerprint, `list permissions` displays it and `revoke FINGERPRINT` removes it.

Ask the assistant to check back at a specified time. One-shot and recurring follow-ups survive a gateway restart. Low-priority digest items use `timezone` and `digestHour` (24-hour time). To enable periodic inbox and calendar checks, connect those accounts in this instance and set `monitorInboxAndCalendar` to true and `defaultChannel` to the paired return channel. The default local setup uses the CLI delivery queue. Operator-defined `watches` take `id`, `instruction`, `minutes`, and `channel`.

## Verify

```sh
KAGEHA_LOOP_BIN="$PWD/target/release/kageha-loop" npm --prefix integrations/personal test
```

This exercises admission and steering against an isolated real Rust backend while one turn is occupied for sixty seconds. Carrier tests use signed local callbacks and a simulated recipient. For live acceptance, use a designated recipient on the enrolled account and check the reply and carrier receipt. Uncertain sends after a crash stay recorded for reconciliation.

To exercise browser approval and delivery from both WhatsApp and the web UI, run these commands from the repository root. The test starts disposable Chromium profiles, a Rust backend, a browser daemon, and the personal gateway. A local socket stands in for the linked phone. Set `KAGEHA_CHROMIUM` to your Chromium executable if it differs from the default macOS Chrome path.

```sh
cargo build --release -p kageha-loop -p kageha-browser
npm --prefix integrations/personal ci
npm --prefix integrations/whatsapp-qr ci
npm --prefix frontend ci
npm --prefix frontend run build
KAGEHA_BROWSER_BIN="$PWD/target/release/kageha-browser" npm --prefix integrations/personal run test:browser
```

### Link the owner's existing WhatsApp or iMessage account

For WhatsApp self-chat, run `npm --prefix integrations/whatsapp-qr ci` from the repository root, set `whatsappSelfChat` to true in the private profile, and restart the gateway. Open `whatsapp-device/pair.html` in the instance directory on the Mac. Scan it from **Linked devices → Link a device** in WhatsApp. The private page refreshes the code and shows connection status. You can send a document or photo up to 16 MiB with a caption, or send the file first and your instructions in the next message. In **Message yourself**, send ordinary text such as `hi` or `status`. The `Kageha:` prefix is optional. Replies use the same self-chat. Store the device directory with the same protections as the private profile.

For iMessage self-chat, set `selfChat` to true, `owner` to the exact Messages address, and `chatId` to 0 in the private bridge configuration. Set `macOwner` to the same address in the gateway. The listener resolves the direct iMessage conversation and processes new outgoing messages starting with `Kageha:`. If the conversation is new, create it first and send the command after the listener connects. The bridge needs Messages database access and macOS permission to send through Messages.

### Agents and results on messaging channels

The gateway uses the Rust harness’s normal session API and the registered artifacts in each completed turn. Text `/agents` for the directory or `/agent sterling draft a brief` to address a particular agent. A gateway can connect to your main Kageha instance in addition to the personal default. Set `agentBackend` to its origin and `agentTokenFile` to its private API token file. Follow-ups and artifact downloads use the backend that owns the task. If the shared server has no default model, set `agentFallbackModel` to a ready model ID for channel turns.

For Gemini Nano Banana, register a media provider on the personal Rust backend through its authenticated `POST /api/media-providers` endpoint. Use ID and protocol `gemini`, base URL `https://generativelanguage.googleapis.com/v1beta`, and set `models.image` to an enabled image model such as `gemini-3.1-flash-image`. Supply the Google API key through the private setup request. The backend stores it in the credential vault. Agents can then call `generate_image` and the resulting image uses ordinary attachment delivery.

Completed outputs arrive as WhatsApp images, audio, video, or named documents, depending on the format. Markdown includes a chat preview. The Mac bridge sends iMessage attachments, carrier WhatsApp uses media URLs, and SMS receives download links. Send `/files #9` to retrieve the files from an earlier task. Individual files up to 64 MiB are supported.

For phone-accessible webpages, have the agent create self-contained HTML in `outputs/`. Set `shareOrigin` to a dedicated HTTPS origin and `sharePort` to its loopback upstream port, forwarding only that listener. Alternatively, set `quickShareTunnel` to true and run `node integrations/personal/service.mjs install-share ~/.kageha-personal` with cloudflared installed. The tunnel has its own result-only listener. Anyone who has an individual link can open it while the instance is online. A quick tunnel gets a new hostname when its process restarts. Send `/files #N` for a fresh link, or use a fixed origin for stable links.
