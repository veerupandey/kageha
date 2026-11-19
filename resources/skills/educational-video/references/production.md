# Production recipes

Set `SKILL` to this skill's absolute directory and `PROJECT` to the task's absolute output directory. The scripts never need to write into the installed skill. Scaffold commands require a new or empty directory to protect existing work.

```bash
python "$SKILL/scripts/video_project.py" doctor
python "$SKILL/scripts/video_project.py" init "$PROJECT" --engine manim
python "$SKILL/scripts/video_project.py" setup "$PROJECT"
python "$SKILL/scripts/video_project.py" render "$PROJECT" --draft
```

Use `--engine remotion` for a React project. Edit the generated sources and `project.json` before rendering. `setup` installs only the chosen engine into `.venv` or `node_modules`, locks resolved dependencies, and can download a rendering browser. It does not install OS packages. Missing native dependencies: follow current official platform documentation, or choose a suitable available renderer. Manim may require Cairo/Pango and pkg-config; math rendered with Tex additionally needs LaTeX (the starter avoids that dependency). FFmpeg/ffprobe are needed for media helpers. Potrace is optional and used only for raster tracing.

## Manim

Use for mathematical concepts, graphs, geometric transformations, network diagrams, and vector reveals. The starter `scene.py` renders `Lesson`, with a tiny gradient-descent example or the SVG named in `project.json`. The example's quadratic is illustrative; replace it with the requested teaching content. Use `Create` for authored strokes, `DrawBorderThenFill` for filled traced regions, `Transform` for a meaningful change, and `LaggedStart` for purposeful sequencing. Scale to both frame dimensions, especially portrait. Draw paths in semantic order; tracing order rarely equals teaching order.

```bash
# Trace a sketch (run with a Python environment containing Pillow):
"$PROJECT/.venv/bin/python" "$SKILL/scripts/trace_image.py" input.png "$PROJECT/drawing.svg" --colors 8
# Set svg="drawing.svg" in project.json, then render.
python "$SKILL/scripts/video_project.py" render "$PROJECT"
```

Trace helper: alpha is composited over the requested background; monochrome uses a configurable threshold; palette mode retains all quantized layers including background. It does not infer semantic objects or centerlines. For photographic subjects consider simplifying the source, tracing fewer colors, or authoring key paths. Preserve intentional background and palette when adapting the scene. A final-image opening hold is optional, not a universal requirement.

## Remotion

Use for narrated explainers, typography, code walkthroughs, captions, composed media, and reusable layouts. The starter uses `Composition`, `Sequence`, frame-driven SVG strokes, optional per-scene audio, and a caption track from JSON. Edit `src/index.tsx` for real diagrams; the initial doodle is a runnable example, not a generic visual adequate for every lesson.

`project.json`: width/height/fps and `scenes` with `seconds`, `title`, `narration`, optional `audio` (a filename in `public/`). Scene duration must accommodate measured narration. Optional `captions` is an array of `{start, end, text}` in absolute seconds. Imported JSON determines composition duration; rerender after editing. Animations must derive from `useCurrentFrame()` rather than CSS transitions, timers, or unseeded randomness. Use Remotion media components for asset loading and pin all Remotion packages to the same version. Keep `package-lock.json` with sources. Check the current license if the intended use requires it; do not assume every renderer has identical terms.

## Audio, assembly, captions, review

Use `gemini_tts(text=..., filename="artifacts/video/<slug>/narration-01.wav")` when available. It is an agent tool, not a shell function. Never call a nonexistent provider from a helper. Generate scene-level audio, measure it with `media.py probe`, then set durations and animation beats; do not clip the last sentence with `-shortest`.

```bash
python "$SKILL/scripts/media.py" probe narration-01.wav
python "$SKILL/scripts/media.py" mux silent.mp4 narration.wav narrated.mp4
python "$SKILL/scripts/media.py" concat part1.mp4 part2.mp4 --output final.mp4
python "$SKILL/scripts/media.py" captions timed-segments.json captions.srt
python "$SKILL/scripts/media.py" inspect final.mp4 --audio --width 1920 --height 1080 --duration 60 --frames review
```

`mux` replaces audio explicitly, pads a shorter narration with silence, and refuses narration longer than the video. Extend/rerender the visual before retrying. `concat` normalizes dimensions, fps and codecs, retains audio, and supplies silence for silent segments. `captions` requires a sorted non-overlapping array of `{start,end,text}`; use recorded sentence boundaries or forced alignment of actual speech. A whole scene's narration in one cue is usually too much text. Inspect captions against audio and provide an SRT sidecar or render the same cues in Remotion. FFmpeg can mux SRT into MP4 using `mov_text` if requested.

`inspect` fully decodes the video and saves representative frames, including near the ending. It fails on missing expected streams, mismatched dimensions or duration. Open its PNGs with an image-capable tool. These are technical checks, not an automated judgment of teaching quality. Review each scene and transitions in addition to samples on longer videos.

Other tools are welcome: Blender for spatial mechanisms, Canvas/SVG for bespoke doodles, screen recording for real UI demonstrations, FFmpeg for assembly, available generation tools for illustrative clips. Match the requested output; a simple wipe over a still is not a hand-drawn path animation. Preserve editable source and repeatable commands whichever tool is used.

Official references:
- https://docs.manim.community/en/stable/installation.html
- https://docs.manim.community/en/stable/reference/manim.mobject.svg.svg_mobject.SVGMobject.html
- https://voiceover.manim.community/en/stable/quickstart.html (optional voiceover integration)
- https://www.remotion.dev/docs/cli/render
- https://www.remotion.dev/docs/animating-properties
- https://www.remotion.dev/docs/html5-audio
- https://www.remotion.dev/docs/captions/
- https://ffmpeg.org/ffmpeg.html
- https://potrace.sourceforge.net/
