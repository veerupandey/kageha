# Browser setup and control

## Start with a private profile

Open **Browser** in a chat, or ask Kageha to open a website. The private Chromium session is the default for interactive work in the Web UI. Install Chrome or Chromium on the host. With a release bundle, set `KAGEHA_CHROMIUM` to the full path of its executable when discovery cannot find it. The [Linux deployment guide](LINUX_DEPLOYMENT.md#enable-a-linux-browser-or-desktop) covers managed headless and desktop sessions.

## Connect a signed-in browser

Run a dedicated debug-enabled Chrome or Comet profile. Configure the Kageha host's `KAGEHA_CHROME_CDP_ENDPOINT` or `KAGEHA_COMET_CDP_ENDPOINT` with its local CDP address and restart the host. Keep the endpoint on loopback. Choose that profile in the Browser panel and approve access from the chat. Tabs and sign-in in the connected profile are separate from the private Chromium session.

Use **Take control** to click, type, or sign in yourself. Enter passwords and one-time codes directly on the website. Return control from the Browser panel when you want Kageha to continue.

## Browser decisions and artifacts

In **Settings → Decisions**, connect a supported decision model and test it before enabling the route. Compare mode records proposals for review. Active mode lets the native executor validate supported actions against the observed page and current approvals. Review the route indicator when work falls back to the regular agent model. See [browser decision configuration](../crates/kageha-browser-decisions/README.md).

Attach uploads to the chat first. Review the destination and file when Kageha asks for approval. Completed downloads appear as chat artifacts. Check the page and the recorded tool result before retrying a possibly dispatched action.

For service flags, protocol details, and integration tests, see the [browser runtime](../crates/kageha-browser/README.md) and the [standalone guide](../crates/kageha-browser/STANDALONE.md).
