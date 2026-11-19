export type MediaModelId = "auto" | "codex" | "nano-banana" | "fal" | "antigravity" | "cloudflare" | "openai";

export interface MediaModelOption {
  id: MediaModelId;
  label: string;
  detail: string;
  capabilities: string[];
  available: boolean;
}

export const MEDIA_MODEL_OPTIONS: MediaModelOption[] = [
  { id: "auto", label: "Auto", detail: "Native first, then the saved image provider", capabilities: ["smart routing"], available: true },
  { id: "openai", label: "OpenAI · GPT Image", detail: "Generate and edit using your existing OpenAI connection", capabilities: ["generate", "edit"], available: true },
  { id: "codex", label: "Codex Native", detail: "Codex runtime native image tool", capabilities: ["generate", "edit"], available: true },
  { id: "nano-banana", label: "Nano Banana", detail: "Explicit Gemini image generation", capabilities: ["generate", "edit"], available: true },
  { id: "fal", label: "Fal · Flux", detail: "Explicit Fal image models", capabilities: ["generate", "edit"], available: true },
  { id: "antigravity", label: "Antigravity Native", detail: "Native generate_image tool", capabilities: ["generate", "edit", "vision"], available: true },
  { id: "cloudflare", label: "Cloudflare · Flux", detail: "Workers AI flux-1-schnell / lucid-origin", capabilities: ["generate"], available: true },
];

export function mediaModelLabel(id: string): string {
  return MEDIA_MODEL_OPTIONS.find((option) => option.id === id)?.label || "Auto";
}
