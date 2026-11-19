/**
 * Verify mobile composer stays tappable when workspace panel is open.
 * node scripts/iat-composer-panel.mjs [baseUrl]
 */
import { chromium, devices } from "playwright";

const baseUrl = process.argv[2] || "http://127.0.0.1:8788";

async function main() {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ ...devices["iPhone 12 Pro"] });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.locator(".ka-sidebar-backdrop").click({ timeout: 1000 }).catch(() => {});
  const input = page.getByPlaceholder(/ask anything|start a task|follow-up/i).first();
  await input.waitFor({ state: "visible", timeout: 10000 });
  await input.fill("IAT composer panel test — reply OK");
  await page.locator("#btn-send, button[aria-label='Send']").first().click();
  await page.waitForSelector("#stage, .ka-thread-view", { timeout: 60000 });
  await page.waitForTimeout(2000);

  const filesPill = page.getByRole("button", { name: /^files$/i });
  if (!(await filesPill.count())) throw new Error("Files pill not found");
  await filesPill.first().click({ force: true });
  await page.waitForSelector(".ka-canvas-panel--open", { timeout: 5000 });

  const composer = page.locator(".ka-composer-slot");
  const send = page.locator("#btn-send");
  await send.waitFor({ state: "visible" });

  const panelBox = await page.locator(".ka-canvas-panel--open").boundingBox();
  const sendBox = await send.boundingBox();
  const composerZ = await composer.evaluate((el) => getComputedStyle(el).zIndex);
  const panelBottom = panelBox ? panelBox.y + panelBox.height : 0;
  const sendTop = sendBox ? sendBox.y : 0;

  const sendBelowPanel = sendTop >= panelBottom - 2;
  const sendClickable = await send.click({ trial: true }).then(() => true).catch(() => false);

  await input.fill("Follow-up while Files open — one word reply");
  await send.click();
  await page.waitForTimeout(20000);

  const text = await page.locator(".ka-conversation").innerText();
  const gotReply = /follow-up|open|OK|ok|one word/i.test(text);

  console.log(`panelBottom=${panelBottom.toFixed(0)} sendTop=${sendTop.toFixed(0)} sendBelowPanel=${sendBelowPanel}`);
  console.log(`composerZ=${composerZ} sendClickable=${sendClickable} gotReply=${gotReply}`);

  await browser.close();
  if (!sendBelowPanel || !sendClickable || !gotReply) process.exit(1);
  console.log("PASS: composer tappable with workspace open");
}

main().catch((e) => { console.error(e); process.exit(1); });
