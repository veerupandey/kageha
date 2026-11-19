// Node 20+, zero dependencies. Run against a disposable standalone API instance.
import { readFile } from 'node:fs/promises';
import { BrowserClient } from './client.mjs';

const token = (await readFile(process.env.BROWSER_API_TOKEN_FILE, 'utf8')).trim();
const browser = new BrowserClient({
  baseUrl: process.env.BROWSER_API_URL ?? 'http://127.0.0.1:8790', token,
  workspaceId: 'demo', clientId: 'my-agent',
});
await browser.connect();
function checked(reply) {
  if (reply.status !== 'ok') throw new Error(JSON.stringify({ error: reply.error, phase: reply.execution_phase, result: reply.result }));
  return reply.result;
}
const launched = checked(await browser.call('launch_browser'));
const tab = checked(await browser.call('open_tab', { browser_id: launched.browser_id, url: process.env.BROWSER_DEMO_URL ?? 'https://example.com' }));
const observation = await browser.call('observe_tab', { browser_id: launched.browser_id, target_id: tab.target_id, compact: false });
console.log(JSON.stringify(observation, null, 2));
// Feed browser.tool() to any agent's tool registry, then forward calls:
// await browser.call(args.method, args.params, {
//   authority: args.authority, timeoutMs: args.timeout_ms,
// });
// Do not automatically repeat a call after a timeout or unknown receipt.
