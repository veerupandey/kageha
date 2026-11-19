---
name: capture_evidence
description: Capture redirect or browsing proof for abuse reports on /machine — HTTP headers, screenshots, optional MP4. Use instead of bash ffmpeg/playwright sidecars.
triggers:
  - record the screen
  - screen recording
  - redirect proof
  - abuse report
  - capture redirect
  - proof video
  - bareandfair.org
allowed-tools: capture_redirect_evidence browser_open browser_screenshot web_fetch bash
---

# capture_evidence

When the user needs **proof that a URL redirects** (registrar abuse, phishing reports, brand impersonation):

## Preferred (one call)

```
capture_redirect_evidence(
  url="http://bareandfair.org/",
  expect_host="bareandfair.ca",
  record_video=true,
  video_seconds=15,
  artifact_prefix="redirect_proof",
)
```

This tool:
- Saves HTTP redirect headers (`*_headers.json`)
- Opens the URL via **browser_open** (harness-owned Chromium — never bash-launch)
- Saves before/after screenshots
- Optionally records an MP4 via **ffmpeg x11grab inside the machine**

Do **not** claim success unless the JSON result has `"ok": true` and `video_ok` when video was requested.

## Fallback (screenshots only)

If video fails, headers + screenshots are usually enough for Hostinger/Zoho/Microsoft abuse forms:
1. `web_fetch` or curl headers for the redirect chain
2. `browser_open(url)` with screenshot
3. Save final URL from the browser snapshot

## Never on /machine

- `pkill chromium`, manual `chromium …`, CDP probes, Playwright sidecar scripts
- `xdotool` / `scrot` for browser control
- Declaring an MP4 deliverable without ffprobe-valid duration

If bash returns `DENIED: do not kill, relaunch, or CDP-probe Chromium`, switch to
`capture_redirect_evidence` — do not retry the same bash pattern.
