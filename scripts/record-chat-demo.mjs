/**
 * Live chat demo for screen recording — headed browser on DISPLAY.
 * node scripts/record-chat-demo.mjs
 */
import { chromium, devices } from "playwright";

const BASE = "http://127.0.0.1:8788";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

async function typeSlow(locator, text, delay = 40) {
  await locator.click();
  for (const ch of text) {
    await locator.pressSequentially(ch, { delay });
  }
}

async function main() {
  const browser = await chromium.launch({
    headless: false,
    slowMo: 80,
    args: ["--window-size=420,900", "--window-position=100,80"],
  });
  const context = await browser.newContext({ ...devices["iPhone 12 Pro"], locale: "en-US" });
  const page = await context.newPage();
  await page.goto(BASE, { waitUntil: "networkidle", timeout: 30000 });
  await pause(2000);

  // New chat from home
  await page.locator(".ka-sidebar-backdrop").click({ timeout: 1000 }).catch(() => {});
  const hero = page.getByPlaceholder(/ask anything|start a task/i);
  await hero.waitFor({ state: "visible", timeout: 10000 });
  await pause(1000);
  const msg1 =
    "Create artifacts/demo_note.md with 2 bullet points about why mobile chat UX matters. Use write_file.";
  await typeSlow(hero, msg1);
  await pause(800);
  await page.locator("#btn-send, button[aria-label='Send']").first().click();
  await pause(15000);

  // Follow-up
  const input = page.locator("#message-input").first();
  await input.waitFor({ state: "visible", timeout: 60000 });
  await pause(1000);
  await typeSlow(input, "Add a third bullet about bottom composer placement.");
  await pause(600);
  await page.locator("#btn-send").click();
  await pause(18000);

  // Open Files + follow-up with panel open
  await page.getByRole("button", { name: /^files$/i }).first().click({ force: true });
  await pause(2000);
  await typeSlow(input, "Summarize all 3 bullets in one sentence.");
  await pause(600);
  await page.locator("#btn-send").click({ force: true });
  await pause(20000);

  await pause(3000);
  await browser.close();
  console.log("Demo complete");
}

main().catch((e) => { console.error(e); process.exit(1); });
