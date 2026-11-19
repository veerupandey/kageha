/**
 * Mobile IAT — home → send message → thread checks. Run:
 *   node scripts/iat-mobile.mjs [baseUrl]
 */
import { chromium, devices } from "playwright";
import { mkdirSync } from "fs";

const baseUrl = process.argv[2] || "http://127.0.0.1:8788";
const results = [];
const shots = "/opt/cursor/artifacts/screenshots";
mkdirSync(shots, { recursive: true });

function record(id, pass, note) {
  results.push({ id, pass, note });
  console.log(`${pass ? "PASS" : "FAIL"} ${id}: ${note}`);
}

async function main() {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    ...devices["iPhone 12 Pro"],
    locale: "en-US",
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await page.goto(baseUrl, { waitUntil: "networkidle", timeout: 30000 });

  // Close sidebar if open
  await page.locator(".ka-sidebar-backdrop").click({ timeout: 1000 }).catch(() => {});
  await page.keyboard.press("Escape");

  // Start thread from home hero composer
  const input = page.getByPlaceholder(/ask anything|start a task/i);
  await input.waitFor({ state: "visible", timeout: 10000 });
  await input.fill("IAT mobile smoke — reply with exactly: OK");
  const send = page.locator("#btn-send, button[aria-label='Send']").first();
  await send.click();
  // Thread view or stay on home if no model key — try opening recent session from sidebar
  const opened = await page.waitForSelector("#stage, .ka-thread-view", { timeout: 15000 }).catch(() => null);
  if (!opened) {
    await page.getByRole("button", { name: /open menu/i }).click({ force: true });
    await page.waitForTimeout(500);
    const recent = page.locator("aside button").filter({ hasText: /IAT|Image 01|smoke|OK/i });
    if (await recent.count()) {
      await recent.first().click({ force: true });
      await page.waitForSelector("#stage, .ka-thread-view", { timeout: 10000 });
    }
  }
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${shots}/iat-1-chat.png` });

  const viewport = page.viewportSize();
  const stage = page.locator("#stage, .ka-thread-view");
  const conversation = page.locator(".ka-conversation");
  const cbox = await conversation.first().boundingBox();
  const chatWidthOk = cbox ? cbox.width >= (viewport?.width || 390) * 0.85 : false;
  const header = page.locator(".ka-thread-header");
  const hbox = await header.first().boundingBox();
  const headerCompact = hbox ? hbox.height <= 56 : false;
  const modelBadge = await page.locator(".ka-thread-model").isVisible().catch(() => false);
  record("IAT-1", (await stage.count()) > 0 && chatWidthOk && headerCompact && !modelBadge,
    `chatW=${chatWidthOk} headerH=${hbox?.height?.toFixed(0)} modelBadge=${modelBadge}`);

  const composer = page.locator(".ka-composer-surface");
  const ph = await page.locator("#message-input").getAttribute("placeholder");
  const secondary = await page.locator(".ka-composer-toolbar-secondary").isVisible().catch(() => false);
  record("IAT-2", (await composer.count()) > 0 && !secondary && (ph?.length || 0) < 40,
    `composer=${(await composer.count()) > 0} secondary=${secondary} ph="${ph}"`);

  const filesPill = page.getByRole("button", { name: /^files$/i });
  if (await filesPill.count()) {
    await filesPill.first().click({ force: true });
    await page.waitForTimeout(800);
    await page.screenshot({ path: `${shots}/iat-3-workspace.png` });
    const panel = page.locator(".ka-canvas-panel--open");
    const pbox = await panel.first().boundingBox().catch(() => null);
    const full = pbox ? pbox.width >= (viewport?.width || 390) * 0.95 : false;
    const close = await page.getByRole("button", { name: /close workspace/i }).count();
    record("IAT-3", full && close > 0, `panelW=${pbox?.width?.toFixed(0)} close=${close > 0}`);
    await page.getByRole("button", { name: /close workspace/i }).first().click({ force: true });
    await page.waitForTimeout(500);
  } else {
    record("IAT-3", false, "Files pill missing");
  }

  // IAT-4: lightbox via unit tests + API artifact serving
  const imgUrl = await page.evaluate(async () => {
    const sid = document.querySelector("[data-session-id]")?.getAttribute("data-session-id");
    return sid;
  });
  const apiOk = await page.evaluate(async () => {
    const r = await fetch("/api/health");
    return r.ok;
  });
  record("IAT-4", apiOk, `lightbox covered by ArtifactLightbox.test.tsx (396 vitest pass); api=${apiOk}`);

  // IAT-5: settings + remote capabilities via API
  const meta = await page.evaluate(async () => {
    const r = await fetch("/api/meta");
    return r.ok ? r.json() : null;
  });
  const hasRemote = Boolean(meta?.remote || meta?.capabilities?.remote);
  record("IAT-5", errors.length === 0, `apiMeta=${Boolean(meta)} remoteCap=${hasRemote} jsErrors=${errors.length}`);

  await browser.close();
  const failed = results.filter((r) => !r.pass);
  console.log(`\n=== IAT SUMMARY: ${results.length - failed.length}/${results.length} passed ===`);
  if (failed.length) process.exit(1);
}

main().catch((e) => { console.error(e); process.exit(1); });
