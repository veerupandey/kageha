---
name: nano_banana
description: >
  Still-image work with Gemini Nano Banana. Parse the user's intent first, then
  pick generate vs edit. Any number of attached images (1…14). Prefer session
  inputs/ over research, downloads, or project artifacts. Covers product swaps,
  in-place edits, composites, scratch generation, carousels, and Instagram
  recreations.
triggers:
  - nano banana
  - generate image
  - generate images
  - edit this image
  - edit the image
  - replace the
  - swap the
  - composite
  - product image
  - product photo
  - packshot
  - image generation
  - create carousel
  - Instagram carousel
  - banana2
  - gemini image
  - attached image
  - look at this
allowed-tools: nano_banana_generate nano_banana_edit gemini_analyze download_file install_python_packages bash web_search web_fetch research_run
---

# nano_banana

Still images only: ads, heroes, packshots, composites, infographics, carousels.
You are multimodal. Session images are already on this user message as pixels
(base64). Look at them. Parse intent **before any tool call**. Run exactly one
workflow. Do not mix.

## 1. Parse intent

Read, in order: (a) this turn's text and the images on **this** message,
(b) images already on earlier user messages in this thread (replayed from
history), (c) `Attached files:` / `inputs/…` named **this turn**. Then pick
**one** row:

| Intent | Signals | Tool |
|---|---|---|
| `ask_for_assets` | "look at this" / "I gave these" / "the attached" but **no** `inputs/` and user did not name a session file | none — ask to re-attach |
| `swap` | 2…N images; replace / swap a product in one or more scenes using other attached packshots | `nano_banana_edit` (one call per base, all refs in that call) |
| `local_edit` | one image; change label, color, remove object, fix text, keep the same scene | `nano_banana_edit` |
| `restyle` | keep the product, change background / lighting / aesthetic | `nano_banana_edit` |
| `composite_new` | combine attached assets into a **new** layout (ad, hero, CTA) | `nano_banana_edit` |
| `generate_with_refs` | new picture, but product/logo must match attached or downloaded refs | `nano_banana_generate` + `reference_images` |
| `generate_scratch` | no product files; "make an image of…" / concept / mood | `nano_banana_generate` (research if a brand is named) |
| `text_card` | quote, infographic, typography-only; no product | `nano_banana_generate`, no refs |
| `carousel` | several slides, swipeable, IG 4:5, "carousel" | `nano_banana_generate` per slide |
| `ig_recreate` | Instagram `/p/` `/reel/` URL plus product images | import post, then generate/edit |
| `iterate` | "wrong image", "same image I have an issue with", "do it again", "not that one" | redo the **previous intent** with the user's files, never the rejected output |

If two rows could apply: **attached product identity wins**. A swap with any
number of images is `swap`, not `generate_scratch`. "Create" does not mean
generate-from-scratch when attachments exist. N>2 is normal — do not ignore
extra files.

### Assign a role to every attached image

Count images on this message plus images already on earlier user messages
in this thread. Cap **14 files per tool call** (Nano Banana limit). If more
are attached, ask which to use, or split into multiple calls — never drop
files silently. Do not `list_dir inputs/` to rediscover old uploads.

Give each file one role from the prompt, filenames, and what is in the frame:

| Role | What it looks like | Where it goes |
|---|---|---|
| `base` | Scene / ad / hero / CTA that must keep layout | **First** path(s). One `nano_banana_edit` per base. |
| `product_ref` | Packshot, editorial, label close-up, lid, side, back of the *replacement* SKU | All of these after the base, same call |
| `style_ref` | Mood, lighting, palette, type treatment — not the SKU | After product refs |
| `logo_ref` | Logo / wordmark lockup | After product refs |
| `do_not_use` | Rejected prior output, wrong SKU, screenshot of the UI | Omit |

Rules:

- User names files → that mapping wins.
- "Replace X with Y": every image that currently shows X = `base`; every image
  of Y (any angle) = `product_ref`. Pass **all** Y angles, not one guess.
- Several ads + one (or many) product photos → each ad is a `base`; product
  photos are shared `product_ref`s. Batch: parallel `nano_banana_edit`, one
  per base, same ref list.
- Several product angles and **one** scene → one edit: `base, ref1, ref2, …`.
- Mixed SKUs (two products in one ad, or "use tin A on slide 1 and tin B on
  slide 2") → split by the user's mapping; do not average two products into one.
- Unclear roles with 3+ files → one short ask ("which is the ad, which are
  product photos?") unless the filenames already split (e.g. `06_cta_*.jpg` vs
  `editorial-*.jpg` / `packshot-*.jpg`).
- Never use `kageha_agent/artifacts/…` or Downloads unless the user pasted
  that exact path.

## 2. Seeing images (multimodal — default)

The driving LLM accepts images. New uploads this turn are pixels on **this
user message**. Images from earlier turns stay on **those user messages** in
history and are replayed as `image_url` / inline data. Follow-ups can be
text-only — you still see the earlier frames. That is normal.

Kageha does **not** dump every `inputs/` file onto every turn.

- **Look at the attached frames** to assign roles (base vs product vs style).
- Do **not** `list_dir`, `bash`, or PIL to "open" an image you already have.
- `gemini_analyze(path="inputs/….jpg", question="…")` is an optional hop for
  tiny label transcription when you are unsure — not a substitute for looking.
- Fal image tools generate/edit; they are not required to *see* the files.

## 3. Asset rules (every intent)

1. **This turn's attachments first**, then images already in thread history.
   `nano_banana_*` resolves `inputs/` against the **session workspace**. Do
   not `list_dir` / `bash` from the project cwd to find them.
2. **Do not hunt the repo.** No project `artifacts/`, no sibling sessions, no
   Downloads, no web scrape, as a stand-in for an attached image.
3. **Missing files → ask.** If the user implied images and none are on this
   message or earlier thread messages, ask them to re-attach. Do not guess.
4. **Never invent a product, logo, or label.** No verified image → omit it or
   ask. Do not "research then draw" a tin the user already photographed.
5. **Never PIL/ImageDraw for the deliverable.** Inspect dimensions only.
6. **Edit then chat.** After the image tool(s) return, reply with the output
   path(s) and stop. Multiple bases → parallel calls, then one reply. No
   Goal/plan theater, no extra generate "just in case".

## 4. Workflows

### `ask_for_assets`

Tell the user you need the scene image(s) and the real product photo(s). Stop.

### `swap` / `local_edit` / `restyle` / `composite_new`

Pass **every** role-assigned file. One call per `base` (parallel if several
bases). `image_paths` is comma-separated, base first, then all product refs,
then style/logo refs:

```
nano_banana_edit(
    prompt="<intent-specific instruction; name each ref's job (front, lid, label, style)>",
    image_paths="inputs/<base>.jpg, inputs/<ref1>.jpg, inputs/<ref2>.jpg, inputs/<ref3>.jpg",
    filename="artifacts/<descriptive>.jpg",
    aspect_ratio="<match that base unless user asked 1:1|4:5|9:16|16:9>")
```

Prompt templates:

- **swap:** Keep this base's composition, crop, lighting, and typography.
  Replace only the specified product with the exact product shown across the
  following refs (use all angles for identity). Do not invent a scene or a
  lookalike label.
- **local_edit:** Keep everything except the requested change. Same crop.
- **restyle:** Keep product identity from the product refs; change only the
  requested look. Style refs bind aesthetic only.
- **composite_new:** Build the requested layout using only the attached
  assets. If several products are attached, place each where the user asked;
  do not merge SKUs.

### `generate_with_refs`

Pass every product/logo/style ref, comma-separated (max 14):

```
nano_banana_generate(
    prompt="<new scene; say which refs are product vs style>",
    reference_images="inputs/<p1>.jpg, inputs/<p2>.jpg, inputs/<logo>.png",
    filename="artifacts/<descriptive>.jpg",
    aspect_ratio="<asked or 4:5 for IG, 1:1 otherwise>")
```

### `generate_scratch` / `text_card`

Research a named brand (`research_run` / `web_fetch`) **only** when there is
no attached product photo. Then `nano_banana_generate`. Text cards skip
research unless the user named a brand system to copy.

### `iterate`

The last output is rejected. Re-run the original intent with **all original
`inputs/`**, not `artifacts/` from the failed turn, unless the user says to
start from that artifact. Do not drop extra refs that were attached last time.

### `carousel`

1. If an IG URL is present → `ig_recreate` import first.
2. If product photos are attached → pass **all** of them as `reference_images`
   on every product slide. Do not download a substitute or pick only one.
3. Else research + `download_file` a real product image, verify >10KB.
4. Generate slides in parallel, 4:5, `filename="artifacts/carousel/slide_N.png"`.
5. Each file must exist and be >50KB.

### `ig_recreate`

Extract the shortcode (path after `/p/`, `/reel/`, `/tv/`). Public posts only.

| Post type | Tool |
|---|---|
| Photo or carousel (`/p/`) | `instaloader` |
| Video or reel (`/reel/`, `/tv/`) | `yt-dlp` |

```bash
python3 -m instaloader --no-captions -- -<shortcode>
python3 -m yt_dlp --no-playlist -o 'artifacts/carousel/reference/%(id)s.%(ext)s' '<url>'
```

`network=true` from the reference directory. Discover real filenames; sort
carousel members by numeric suffix; each file >10KB. Missing importer →
`install_python_packages` once, retry once. 401/429 → say private/rate-limited.
404 → ask the user to check the URL.

Then generate/edit slides with product `inputs/` as identity and imported
frames as layout refs.

## 5. Models and sizing

| Alias | API id | When |
|---|---|---|
| `banana2` (default) | `gemini-3.1-flash-image` | general / carousel |
| `lite` | `gemini-3.1-flash-lite-image` | cheap drafts |
| `pro` | `gemini-3-pro-image` | tight label/text fidelity |
| `legacy` | `gemini-2.5-flash-image` | fallback |

`KAGEHA_NANO_BANANA_MODEL` overrides. Ratios: `1:1`, `4:5` feed/carousel,
`9:16` story/reel, `16:9` landscape. Match the base image on swap/edit unless
the user asked a ratio.

Requires `GEMINI_API_KEY`. `instaloader` / `yt-dlp` optional.
