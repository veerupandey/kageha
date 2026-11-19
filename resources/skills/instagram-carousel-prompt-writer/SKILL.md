---
name: instagram-carousel-prompt-writer
description: Turn a product brief, product images, and an Instagram reference URL into structured, brand-grounded carousel prompts and a finished carousel/Reel export pack.
triggers:
  - create carousel from Instagram reference
  - recreate Instagram carousel
  - carousel prompt writer
  - product images and reference URL
  - Instagram carousel for my product
  - create slides from reference
allowed-tools: instagram_reference_ingest instagram_generate_slide instagram_generate_slides instagram_export_pack image_generate image_generate_batch image_edit image_analyze write_file bash browser_open browser_snapshot download_file
---

# Instagram Carousel Prompt Writer

Use `instagram_reference_ingest` to capture the reference and prepare a resumable
manifest. Generate slides one at a time with `instagram_generate_slide`; it uses the
session's selected Image Generation provider and can be safely retried. Use
`instagram_export_pack` after review. The compatibility `instagram_reference_to_pack`
name performs the same bounded ingest phase only. It is the native
equivalent of ReelAI's Image Studio → Prompt Writer → Add Reference Post →
Generate All Slides flow.

## Workflow

1. Extract the public Instagram reference in ordered slide/frame order.
2. Load the supplied product images as identity references. Never invent
   packaging, labels, logos, or product claims.
3. Write a structured slide plan with creative direction, reference analysis,
   slide roles, copy, design notes, exact image prompts, and negative prompts.
4. Generate every slide with the product and reference images attached.
5. Generate the optional 9:16 Reel from the lead slide.
6. Return the manifest and ZIP artifact. A missing video is reported as
   partial, never as complete.

## Defaults

- 5 slides unless the user requests 3–10.
- 4:5 portrait unless the user asks 1:1 or 9:16.
- Text is included by default; honor an explicit “no text” request.
- Slide 1 is the hook, middle slides deliver distinct value, and the final
  slide is a CTA.
- Reference structure may be adapted, but visible copy is rewritten and the
  result must be original.

Required inputs: `reference_url` and `creative_brief`. Product image paths
belong in `product_images` as a comma- or newline-separated list relative to
the session workspace.
