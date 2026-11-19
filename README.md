# Kageha

**A self-hosted AI assistant with a live browser and a workspace for files.** Give Kageha a task, follow its actions, and open the results from the same chat.

[Watch the demo](https://github.com/veerupandey/kageha/releases) · [Quick start](#quick-start) · [Build from source](#build-from-source) · [MIT License](LICENSE)

In the 48-second demo, Kageha turns a support backlog into a brief, checks an incident in Chromium on a follow-up, and updates the same file.

## What you can do

- **Work in a chat workspace.** Keep conversations, tool activity, approvals, and generated files together.
- **Use a browser or a remote machine.** Run browser tasks in Chromium, connect a Linux host over SSH, or use an optional Docker workstation.
- **Continue on your phone.** Pair a separate Kageha Personal instance with WhatsApp or iMessage to send requests and receive files. [Personal setup guide](https://github.com/veerupandey/kageha/blob/main/integrations/personal/README.md#install-on-another-host)

## Quick start

Download and extract the Kageha archive for your platform from [GitHub Releases](https://github.com/veerupandey/kageha/releases). If no release is available yet, [build an archive from source](#build-from-source).

| Platform | Start and sign in |
| --- | --- |
| **macOS · Apple silicon** | Open **Start Kageha.command**. Once the server is listening, open **Sign in.command** and paste the copied token into the sign-in form. |
| **Linux · x64** | Run `./start` and open [127.0.0.1:8788](http://127.0.0.1:8788). Run `./start token` in a second terminal and paste the token into the sign-in form. |

In **Settings → Models**, connect a model provider or a compatible local endpoint. Select a model and start a chat. Try asking Kageha to create a short checklist and open it from the **Files** panel.

Leave the launcher running while you use Kageha. Press **Ctrl-C** to stop it. The launcher uses `~/.kageha-local` so newer bundles can reuse your chats and settings. Set `KAGEHA_DATA_DIR` to an absolute path for a separate instance. Set `KAGEHA_WEBUI_PORT` to change the port, and use the same settings when opening the sign-in shortcut or running `./start token`.

For supervised Linux installations, HTTPS, SSH hosts, and the optional desktop, see the [Linux deployment guide](docs/LINUX_DEPLOYMENT.md).

## Build from source

Install stable Rust with Cargo, Node.js 24 with npm, Git, and OpenSSL. From a checkout of the repository, run:

```sh
scripts/package_release.sh
```

Extract the archive in `dist/` and follow the [quick start](#quick-start). For a development checkout, build the backend and frontend, then start the Web UI:

```sh
cargo build --locked --release -p kageha-loop
npm --prefix frontend ci
npm --prefix frontend run build
scripts/start_webui.sh start
```

The development server uses [127.0.0.1:8788](http://127.0.0.1:8788) and writes its sign-in token to `.kageha/rust-webui/server.token` by default. Run `scripts/start_webui.sh stop` to stop it.

## Guides

| Guide | What it covers |
| --- | --- |
| [Agents and teams](https://github.com/veerupandey/kageha/blob/main/docs/AGENTS.md) | Set up specialist agents and choose their tools |
| [Browser setup](https://github.com/veerupandey/kageha/blob/main/docs/BROWSER.md) | Configure Chromium and browser sessions |
| [Native macOS computer](https://github.com/veerupandey/kageha/blob/main/crates/kageha-computer/README.md) | Install the desktop helper and grant macOS permissions |
| [Rust components](https://github.com/veerupandey/kageha/blob/main/docs/ARCHITECTURE.md) | Explore the runtime, storage, tools, and service boundaries |
