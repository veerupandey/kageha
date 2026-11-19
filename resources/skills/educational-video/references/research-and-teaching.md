# Research and teaching decisions

Use primary sources: papers and framework documentation for deep learning, standards or original data for technical claims, WHO/UNESCO and public-health authorities for health and sexuality education. Verify version-sensitive commands and claims against current documentation. Keep a claim → source URL mapping; disclose uncertainty rather than filling gaps. A short doodle need not become a research report.

For deep learning, distinguish a useful metaphor from the computation. Label tensor dimensions where useful, verify numerical examples with code, and distinguish training, inference, loss, gradients, and parameter updates. A network animation should show what each signal means, not decorative random pulses. Introduce the example before the equation; explain notation when it first appears.

For sex education, establish age group and learning objective when not supplied. Use accurate, age-appropriate, nonjudgmental language and clinical, non-erotic diagrams. Include consent, boundaries, relationships, or health context when relevant to the requested lesson; avoid stereotypes and shame. Educational anatomy is a legitimate subject. Do not turn general education into individualized diagnosis or treatment. Check local/current guidance for claims dependent on jurisdiction or clinical recommendations.

Narration: roughly 120–150 words/minute is a planning estimate, slower for new notation; final timing comes from the audio. Use one main idea per scene. Animate causal changes, provide a concrete worked example, and hold completed diagrams long enough to inspect. Spoken detail and on-screen paragraphs should not compete. Keep text inside safe margins and readable at the target viewing size; use color plus shape/labels rather than color alone.

Doodle: prefer authored centerline SVG paths for pen-like strokes. Raster tracing yields closed outlines/regions, so drawing those borders is a different effect. Inspect whether the user wants a line reveal, palette fill, or a moving character. A stroke reveal does not produce articulated character animation. Use rigging, authored keyframes, or a suitable video model for actual character motion.

Research pointers (checked 2026-09-11; recheck when version-sensitive):
- Inspiration: https://github.com/leeyeel/Sketch2Motion — raster to SVG to Manim drawing animation, including color layers. This skill's helpers are original implementations, not a vendored copy of that repository.
- https://www.who.int/news-room/questions-and-answers/item/comprehensive-sexuality-education
- https://www.who.int/publications/m/item/9789231002595
- https://docs.manim.community/en/stable/
- https://www.remotion.dev/docs/
