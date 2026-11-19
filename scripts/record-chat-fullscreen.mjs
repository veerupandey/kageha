/**
 * Full-screen mobile chat demo — chromeless app window filling the display.
 */
import { chromium } from "playwright";

const BASE = "http://127.0.0.1:8788";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

async function typeSlow(locator, text, delay = 32) {
  await locator.click();
  for (const ch of text) {
    await locator.pressSequentially(ch, { delay });
  }
}

async function main() {
  const browser = await chromium.launch({
    headless: false,
    slowMo: 50,
    args: [
      `--app=${BASE}`,
      "--start-maximized",
      "--window-position=0,0",
      "--disable-infobars",
      "--no-first-run",
    ],
  });

  const context = browser.contexts()[0] ?? (await browser.newContext());
  const page = context.pages()[0] ?? (await context.newPage());

  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(BASE, { waitUntil: "networkidle", timeout: 30000 });
  await pause(2000);

  await page.locator(".ka-sidebar-backdrop").click({ timeout: 1000 }).catch(() => {});
  const hero = page.getByPlaceholder(/ask anything|start a task/i);
  await hero.waitFor({ state: "visible", timeout: 10000 });
  await pause(1000);

  await typeSlow(
    hero,
    "Create artifacts/demo_note.md with 2 bullet points about why mobile chat UX matters. Use write_file.",
  );
  await pause(700);
  await page.locator("#btn-send, button[aria-label='Send']").first().click();
  await pause(17000);

  const input = page.locator("#message-input").first();
  await input.waitFor({ state: "visible", timeout: 90000 });
  await pause(1000);
  await typeSlow(input, "Add a third bullet about bottom composer placement.");
  await pause(600);
  await page.locator("#btn-send").click();
  await pause(19000);

  await page.getByRole("button", { name: /^files$/i }).first().click({ force: true });
  await pause(3000);
  await typeSlow(input, "Summarize all 3 bullets in one sentence.");
  await pause(600);
  await page.locator("#btn-send").click({ force: true });
  await pause(20000);

  await pause(3000);
  await browser.close();
  console.log("Full-screen demo complete");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
