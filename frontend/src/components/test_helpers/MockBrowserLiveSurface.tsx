import type { ComponentProps, ImgHTMLAttributes } from "react";
import type { BrowserLiveSurface } from "../BrowserLiveSurface";

// Panel tests exercise ownership, input and polling independently of decoding.
// The real transport/decoder and retained canvas have their own tests.
export function MockBrowserLiveSurface({ source, binding: _binding, surfaceRef, onFrame, onFailure, onPaint, ...props }: ComponentProps<typeof BrowserLiveSurface>) {
  return <img {...(props as unknown as ImgHTMLAttributes<HTMLImageElement>)} src={source} alt=""
    ref={node => surfaceRef(node as unknown as HTMLCanvasElement | null)} onLoad={() => { onFrame(); onPaint?.(); }} onError={onFailure} />;
}
