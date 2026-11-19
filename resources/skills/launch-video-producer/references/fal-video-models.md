# Fal video model guide

Use this reference when the default Wan path is not sufficient. Model features and
prices change; verify the linked Fal page at run time before quoting a price or relying
on a new capability. The links below are official Fal model or API documentation.

## Selection guide

| Model | Best strength | Prefer it when | Cost signal | Kageha status |
|---|---|---|---|---|
| Wan 2.7 I2V | Smooth motion, scene fidelity, visual coherence, first/end-frame control, video continuation, optional driving audio | A finished product/keyframe should be animated cheaply and faithfully | Official page lists $0.10/s at 720p and $0.15/s at 1080p | Available as `fal_image_to_video(model="wan")` |
| Kling 3 Standard/Pro | Cinematic visuals, fluid motion, native audio, multi-prompt/storyboard-style control, custom elements | Premium product motion, cinematic camera work, or native sound matters | Check the selected Kling page; price varies by tier/resolution | `kling3` is wired; use only when its strengths are needed |
| Kling 2.6 Pro | Top-tier cinematic image-to-video with native audio | High-quality single-shot cinematic motion when Kling 3 is unavailable | Check official page at run time | Not a separate Kageha alias; do not invent one |
| MiniMax Video 01 | Simple, direct image-to-video conversion | A straightforward motion pass is needed and Wan is unavailable or unsuitable | Check the model page at run time | Available as `minimax` |
| LTX 2.3 | Fast, high-resolution image/text/audio-to-video, up to 20 seconds, last-frame interpolation, 9:16, native audio | Longer clips, portrait output, fast iteration, or cost-efficient 1080p+ is more important than maximum cinematic control | Official page lists $0.08/s at 1080p, $0.16/s at 1440p, $0.32/s at 2160p | Documented but not wired into Kageha yet |
| Seedance 2.0 | Native audio, director-level camera control, realistic physics, start/end frames, multi-shot and multimodal references | Multi-shot stories, complex interactions, or several reference images/video/audio inputs | Official page lists approximately $0.2419/s fast and $0.3024/s standard at 720p | Documented but not wired into Kageha yet |

## Practical policy

1. Start with Wan 2.7 for a ready-made product image. Its image-to-video API supports
   a first image, optional end image, optional video continuation, and driving audio.
2. Move to Kling when the brief specifically asks for cinematic camera motion, native
   audio, or stronger premium motion quality.
3. Move to Seedance only for real multi-shot or multimodal reference work; it is
   substantially more expensive than Wan and should not be used for a simple product
   animation.
4. Consider LTX for longer or higher-resolution production where its documented price
   and fast/open model characteristics fit the task.
5. Use MiniMax as a simple fallback, not as a reason to run multiple parallel tests.
6. Treat all listed prices as approximate provider pricing. Never present them as the
   user's actual bill without checking the current Fal account/model page.

## Official documentation

- [Fal video generation overview](https://fal.ai/docs/model-api-reference/video-generation-api/overview)
- [Wan 2.7 image-to-video API](https://fal.ai/models/fal-ai/wan/v2.7/image-to-video/api)
- [Wan 2.7 reference-to-video](https://fal.ai/models/fal-ai/wan/v2.7/reference-to-video)
- [Kling 3 Standard image-to-video](https://fal.ai/models/fal-ai/kling-video/v3/standard/image-to-video)
- [Kling 2.6 Pro API](https://fal.ai/docs/model-api-reference/video-generation-api/kling-video-v2.6-pro)
- [MiniMax Video 01 image-to-video](https://fal.ai/models/fal-ai/minimax/video-01/image-to-video)
- [LTX 2.3 image-to-video](https://fal.ai/models/fal-ai/ltx-2.3/image-to-video)
- [Seedance 2.0 image-to-video](https://fal.ai/models/bytedance/seedance-2.0/image-to-video)
- [Seedance 2.0 reference-to-video](https://fal.ai/models/bytedance/seedance-2.0/reference-to-video)
