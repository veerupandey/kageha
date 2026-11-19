---
name: educational-video
description: Create finished video tutorials, animated explainers, whiteboard animations, and topic-based doodles from a brief, sketch, or reference. Research and storyboard the lesson, choose Manim, Remotion, or another suitable renderer, synchronize narration and captions, and deliver verified media plus editable sources.
triggers:
  - create a video tutorial
  - video tutorial
  - educational video
  - animated explainer
  - explain with animation
  - doodle
  - whiteboard animation
  - sketch to motion
  - manim
  - remotion
metadata:
  legacy_python_helpers: true
---

# Educational video

Turn the topic into a finished, understandable video, not just a script or a slideshow of paragraphs. For a static doodle request, deliver an SVG/PNG instead of imposing a video. For product advertising, prefer the product-video-producer skill. No fixed tool allowlist: select available tools by the visual and teaching requirements.

## Autonomy and scope

Choose the storyboard, visual metaphor, renderer, local dependencies, layout, pacing, and ordinary repair steps without asking the user to approve each decision. Honor explicit choices. Use sensible defaults when omitted: beginner audience, user's language, 60–90 seconds for a short explainer, landscape 1920×1080 at 30 fps, narration plus captions for tutorials, and 6–12 seconds with no narration for a doodle reveal. State material assumptions briefly and continue. Ask only if missing information changes the substance (for example the age group of a sexuality-education lesson); research and technical preparation can proceed meanwhile.

Autonomy does not authorize publishing, paid subscriptions, voice impersonation, or sending private inputs to a new external service. Use configured media tools within the user's existing authorization; otherwise choose a local option or identify the specific missing capability. Never claim a draft, silent fallback, or failed render is the promised finished video.

## Build the lesson

1. Inspect supplied images/video/files. Identify learning objective, audience, prerequisite knowledge, format, and duration. For tutorials, choose one outcome the viewer should be able to explain or do afterward.
2. Research factual claims before writing narration. Read [research-and-teaching.md](references/research-and-teaching.md) for sourcing, technical verification, and health/sexuality education. Record relevant URLs and claim notes in the project, proportionally to the task.
3. Write a concise narration and a scene plan: hook/question → concrete example → explanation or worked steps → takeaway. Each scene should say what changes on screen and why that change explains the spoken point. Include seconds, narration, visual action, source references, and renderer. Do not force this sequence onto a simple doodle.
4. Choose and scaffold a renderer using [production.md](references/production.md). Helpers are conveniences, not a closed framework. Write custom scenes or use Blender, Motion Canvas, SVG/Canvas, FFmpeg, a screen capture, or an available generation tool when appropriate. Combine tools if needed; keep factual labels and equations in deterministic overlays.
5. For narration, use Kageha's configured `gemini_tts` when available (one file per scene is easier to repair). Inspect the actual audio duration before finalizing scene lengths. Other available TTS or recorded speech is allowed. Do not shorten the lesson merely to fit one TTS request; chunk at sentence boundaries. Keep narration text in the project.
6. Render a low-resolution representative scene, inspect it, then produce all scenes. Correct factual errors, unreadable labels, unintended overlap, abrupt timing, and missing assets. Reuse successful assets and rerender affected scenes. After repeated identical failures, change the approach rather than looping. For full renders use `submit_media_jobs` with a typed render manifest (argv, staged inputs and declared outputs), then `wait_agents` for a durable completion receipt. Do not launch repeated `nohup`/`setsid` workers or sleep-poll shell loops; they can overlap after retries. Use the user-selected execution target and measured CPU quota, not the host's logical CPU count, when sizing concurrency.
7. Assemble audio, scenes, and captions. Use measured audio timings; do not present estimated word timestamps as aligned captions. The helper can create sentence captions from explicitly timed segments. Keep exact equations, technical terms, and anatomical terminology readable and correct.
8. Validate the final export with `media.py inspect`; view sampled frames and review the narration/timing where playback or audio tools are available. Include the start, a busy frame, transitions, and the ending. Metadata alone does not establish visual or teaching quality. Check expected duration, aspect, complete decoding, audio when promised, and subtitle sync. Record unverified checks honestly.

## Deliverables

Work under `artifacts/video/<slug>/`. Deliver a playable MP4 (H.264/yuv420p, AAC when narrated), captions for narrated lessons, and editable source files with the render command/dependency lock. Preserve the concise script/scene plan and sources beside the output; avoid overwhelming the user with every intermediate. Return a direct media link or preview and a short note about the chosen tools and any material limitation. For a requested static doodle, deliver the image/vector and source instead.

Read only the needed references. `scripts/video_project.py` scaffolds and installs project-local renderer dependencies; `scripts/trace_image.py` makes monochrome or palette-layer SVGs; `scripts/media.py` measures, assembles, captions, and checks outputs. Run each with `--help`. Paths below are relative to this skill directory, not the user's task directory.
