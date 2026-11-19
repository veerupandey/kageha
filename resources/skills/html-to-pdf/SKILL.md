---
name: html-to-pdf
description: Convert HTML files to beautiful styled PDFs. Prefer the native html_to_pdf tool. On a task machine it uses box Chromium --print-to-pdf (not host Chrome, not Playwright). On the host, try elevated Chrome headless, then Playwright, PyMuPDF Story, and fpdf2. Use when the user asks to convert HTML to PDF, export a document as PDF, make a PDF from an HTML file, or when a previous HTML→PDF attempt failed.
license: MIT
allowed-tools: html_to_pdf bash read_file
metadata:
  legacy_python_helpers: true
---

# HTML → PDF Conversion

Convert a standalone HTML file (with inline `<style>`) into a faithful, styled PDF. Preserves gradients, tables, code blocks, colors, and page layout.

## CRITICAL: The escalation ladder (try in order)

```
0. Native html_to_pdf tool                   ← FIRST always
1. Task machine Chromium --print-to-pdf      ← if the tool is missing
2. Elevated host Chrome headless --print-to-pdf
3. Playwright sync_playwright page.pdf()      ← host only; never pip-install in the box
4. PyMuPDF (fitz) Story engine                 ← low fidelity; can hang
5. fpdf2 write_html()                          ← last resort; loses most styling
```

Call `html_to_pdf(html_path, pdf_path)` before any bash Chromium. The tool already uses a throwaway `--user-data-dir` and never `--remote-debugging-port`.

When this session is a task machine (`KAGEHA_MACHINE=1`, project `/work`), **do not** call Mac `/Applications/Google Chrome`, `bash(elevated=True)`, or `playwright install`. Chromium is already at `/usr/bin/chromium`. Headless `--print-to-pdf` with a **temp** `--user-data-dir` is allowed. Launching Chromium with `--remote-debugging-port`, `pkill`, or xdotool is denied (that hijacks the Live headed browser).

## Step 0 — Native tool (always)

```
html_to_pdf(html_path="artifacts/input.html", pdf_path="artifacts/output.pdf")
```

- On `/machine`, this is box Chromium. On the host, this is Chrome/Chromium if present.
- Do **not** pass `--remote-debugging-port` or point `--user-data-dir` at `/session/.chromium`.
- If the tool returns a PDF that starts with `%PDF-`, stop. Do not "also try" Playwright in the box.

## Step 1 — Task machine bash fallback (KAGEHA_MACHINE=1)

```bash
TMPDIR_PDF=$(mkdir -p "${TMPDIR:-.}/kageha-pdf" && mktemp -d "${TMPDIR:-.}/kageha-pdf/chrome.XXXXXX")
/usr/bin/chromium --headless --disable-gpu --no-sandbox \
  --user-data-dir="$TMPDIR_PDF" \
  --print-to-pdf="$KAGEHA_ARTIFACTS/output.pdf" \
  --print-to-pdf-no-header \
  --no-pdf-header-footer \
  --virtual-time-budget=5000 \
  "file://$KAGEHA_ARTIFACTS/input.html"
```

- Do **not** pass `--remote-debugging-port` or point `--user-data-dir` at `/session/.chromium`.
- Guest HTML path is `/session/artifacts/...`, not a macOS `/Users/...` path.
- Validates: `head -c 5 "$KAGEHA_ARTIFACTS/output.pdf"` prints `%PDF-`.
- If this is denied, the command still contains a hijack token — drop it; do not fall through to Playwright-in-the-box.

## Step 2 — Host only: Elevated Chrome headless (off-machine)

```bash
CHROME=$(ls /Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome 2>/dev/null || which chromium || which google-chrome)
TMPDIR_PDF=$(mkdir -p "${TMPDIR:-.}/kageha-pdf" && mktemp -d "${TMPDIR:-.}/kageha-pdf/chrome.XXXXXX")
"$CHROME" --headless --disable-gpu --no-sandbox \
  --user-data-dir="$TMPDIR_PDF/profile" \
  --print-to-pdf="$KAGEHA_ARTIFACTS/output.pdf" \
  --print-to-pdf-no-header \
  --no-pdf-header-footer \
  "file://$KAGEHA_ARTIFACTS/input.html"
```

- Run this via `bash(elevated=True)` — it WILL prompt the user for approval because it's a host escape. That's expected and necessary.
- Must pass `--user-data-dir` pointing at a fresh mktemp directory. Without it, Chrome tries to write to the default profile path which the sandbox blocks.
- Must pass `file://` absolute path to the HTML.
- Validates: `head -c 5 output.pdf` should print `%PDF-`.

See `scripts/render_chrome.sh` for a self-contained, reusable version. Run it with `bash(elevated=True)`, not `skill_run` — skill_run stays inside the OS sandbox.

## Step 3 — FALLBACK: Playwright (only if Chrome binary unavailable)

```python
from playwright.sync_api import sync_playwright
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, args=["--no-sandbox", "--disable-gpu"])
    page = browser.new_context().new_page()
    page.goto(f"file://{HTML_PATH}", wait_until="networkidle", timeout=30000)
    page.wait_for_timeout(1500)
    page.pdf(path=PDF_PATH, format="A4", print_background=True,
             margin={"top":"12mm","bottom":"12mm","left":"10mm","right":"10mm"}, scale=0.82)
    browser.close()
```

**Known failure mode:** Crashes with `Executable doesn't exist at .../chrome-mac/headless_shell` or `ProcessStartupError` when the sandbox blocks profile-dir writes. If you see this, jump to Step 2 (elevated Chrome).

See `scripts/render_playwright.py`.

## Step 4 — FALLBACK: PyMuPDF Story (pure Python, no browser)

```python
import fitz
story = fitz.Story(html=body_html, user_css=SIMPLE_CSS)  # strip <style>/<script> first
writer = fitz.DocumentWriter(str(PDF_OUT))
more = True
while more:
    dev = writer.begin_page(fitz.Rect(0, 0, 595, 842))
    more, _ = story.place(fitz.Rect(28, 28, 567, 814))
    story.draw(dev)
    writer.end_page()
writer.close()
```

**Known failure modes:**
- **HANGS** if the HTML contains complex/nested `<style>` blocks or certain flex/grid CSS. Always strip `<style>` and `<script>` tags and pass a `user_css` string instead. Wrap in `signal.alarm(25)` so it can't hang forever.
- Renders only basic CSS — no gradients, limited table styling, no `@media print`.

See `scripts/render_fitz.py`.

## Step 5 — LAST RESORT: fpdf2

```python
from fpdf import FPDF, HTMLMixin
class P(FPDF, HTMLMixin): pass
pdf = P(format="A4"); pdf.add_page()
pdf.write_html(body_html)  # very basic HTML support
pdf.output(PDF_PATH)
```

Loses almost all styling (colors, gradients, code highlighting). Use only when nothing else works and the user just needs *a* PDF.

See `scripts/render_fpdf2.py`.

## Verification (always run after generating)

```bash
head -c 5 "$KAGEHA_ARTIFACTS/output.pdf"   # must print: %PDF-
python3 -c "import fitz; d=fitz.open('$KAGEHA_ARTIFACTS/output.pdf'); print(f'Pages: {d.page_count}, Page1: {d[0].get_text()[:80]}')"
```

## Failure log

See `references/failures.md` for the documented failure modes encountered in the real session that produced this skill — including exact error messages and root causes. Read this BEFORE debugging a failed PDF render; the answer is likely already there.

## Quick decision table

| Situation | Use |
|:---|:---|
| Any session | **Step 0** (`html_to_pdf` tool) |
| Tool missing, task machine (`KAGEHA_MACHINE=1`) | **Step 1** (box Chromium `--print-to-pdf`) |
| Chrome installed on host, no machine | **Step 2** (elevated Chrome) |
| No Chrome, Playwright installed on host | Step 3 (Playwright) |
| No browser at all, PyMuPDF installed | Step 4 (fitz Story) |
| Nothing else works | Step 5 (fpdf2) |
| `uv pip install` fails with cache errors | Don't retry pip — Step 0 or Step 1 |
