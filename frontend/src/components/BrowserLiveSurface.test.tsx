import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { BrowserLiveSurface } from "./BrowserLiveSurface";
const draw = vi.fn();
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); draw.mockClear(); });

function transport() {
  const requests: ReadableStreamDefaultController<Uint8Array>[] = [];
  const fetcher = vi.fn(async (_url, options) => {
    const body = new ReadableStream<Uint8Array>({ start(controller) {
      requests.push(controller);
      options.signal.addEventListener("abort", () => { try { controller.error(new DOMException("Aborted", "AbortError")); } catch {} });
    }});
    return new Response(body);
  });
  vi.stubGlobal("fetch", fetcher);
  vi.stubGlobal("createImageBitmap", vi.fn(async () => ({ width: 800, height: 600, close: vi.fn() })));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage: draw } as unknown as CanvasRenderingContext2D);
  const emit = (index: number) => requests[index].enqueue(new TextEncoder().encode("--frame\r\nContent-Length: 4\r\n\r\njpeg\r\n"));
  return { fetcher, requests, emit };
}

it("keeps the same decoded canvas through source changes and clears only a different target", async () => {
  const t = transport(), onFrame = vi.fn(), onFailure = vi.fn(), surfaceRef = vi.fn();
  const view = render(<BrowserLiveSurface source="/one" binding="tab:1" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.requests).toHaveLength(1));
  t.emit(0); await waitFor(() => expect(onFrame).toHaveBeenCalledTimes(1));
  const canvas = view.container.querySelector("canvas")!;
  expect(canvas.width).toBe(800);
  view.rerender(<BrowserLiveSurface source="/two" binding="tab:1" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.requests).toHaveLength(2));
  expect(view.container.querySelector("canvas")).toBe(canvas);
  expect(canvas.width).toBe(800);
  expect(draw).toHaveBeenCalledTimes(1);
  t.emit(1); await waitFor(() => expect(onFrame).toHaveBeenCalledTimes(2));
  view.rerender(<BrowserLiveSurface source="/three" binding="tab:2" {...{surfaceRef,onFrame,onFailure}} />);
  expect(canvas.width).toBe(1);
  expect(onFailure).not.toHaveBeenCalled();
});

it("detects EOF and preserves the last frame while reconnecting without callback-driven requests", async () => {
  const t = transport(), onFrame = vi.fn(), onFailure = vi.fn(), surfaceRef = vi.fn();
  const view = render(<BrowserLiveSurface source="/one" binding="tab:1" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.requests).toHaveLength(1));
  t.emit(0); await waitFor(() => expect(onFrame).toHaveBeenCalled());
  const canvas = view.container.querySelector("canvas")!;
  t.requests[0].close(); await waitFor(() => expect(onFailure).toHaveBeenCalledTimes(1));
  view.rerender(<BrowserLiveSurface source="" binding="tab:1" surfaceRef={surfaceRef} onFrame={vi.fn()} onFailure={vi.fn()} />);
  expect(canvas.width).toBe(800);
  expect(t.fetcher).toHaveBeenCalledTimes(1);
  view.rerender(<BrowserLiveSurface source="/retry" binding="tab:1" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.fetcher).toHaveBeenCalledTimes(2));
  view.rerender(<BrowserLiveSurface source="/retry" binding="tab:1" surfaceRef={surfaceRef} onFrame={vi.fn()} onFailure={vi.fn()} />);
  expect(t.fetcher).toHaveBeenCalledTimes(2);
  expect(canvas.width).toBe(800);
});

it("discards a late decoded frame from the previous profile target", async () => {
  const t = transport(), onFrame = vi.fn(), onFailure = vi.fn(), surfaceRef = vi.fn();
  let finishOld!: (value: ImageBitmap) => void;
  vi.mocked(createImageBitmap).mockImplementationOnce(() => new Promise(resolve => { finishOld = resolve; }));
  const view = render(<BrowserLiveSurface source="/old" binding="profile:old" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.requests).toHaveLength(1));
  t.emit(0); await waitFor(() => expect(createImageBitmap).toHaveBeenCalledTimes(1));
  view.rerender(<BrowserLiveSurface source="/new" binding="profile:new" {...{surfaceRef,onFrame,onFailure}} />);
  await waitFor(() => expect(t.requests).toHaveLength(2));
  t.emit(1); await waitFor(() => expect(onFrame).toHaveBeenCalledTimes(1));
  const close = vi.fn();
  finishOld({width: 1920, height: 1080, close} as unknown as ImageBitmap);
  await waitFor(() => expect(close).toHaveBeenCalled());
  expect(draw).toHaveBeenCalledTimes(1);
  expect(view.container.querySelector("canvas")!.width).toBe(800);
  expect(onFrame).toHaveBeenCalledTimes(1);
  expect(onFailure).not.toHaveBeenCalled();
});

it("reports each painted frame only after decoding and drawing succeeds", async () => {
  const t = transport(), onPaint = vi.fn();
  render(<BrowserLiveSurface source="/paint" binding="tab:paint" surfaceRef={vi.fn()}
    onFrame={vi.fn()} onFailure={vi.fn()} onPaint={onPaint} />);
  await waitFor(() => expect(t.requests).toHaveLength(1));
  expect(onPaint).not.toHaveBeenCalled();
  t.emit(0);
  await waitFor(() => expect(onPaint).toHaveBeenCalledTimes(1));
  expect(draw).toHaveBeenCalledTimes(1);
  t.emit(0);
  await waitFor(() => expect(onPaint).toHaveBeenCalledTimes(2));
  expect(draw).toHaveBeenCalledTimes(2);
});
