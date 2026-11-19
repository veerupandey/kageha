import { expect, it } from "vitest";
import { browserFrames } from "./browserFrames";
const text = (value: string) => new TextEncoder().encode(value);
const part = (value: string) => text(`--kageha-frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${text(value).length}\r\n\r\n${value}\r\n`);

it("reads binary payloads across split headers and coalesced parts", async () => {
  const first = part("jpeg\r\n\r\n--kageha-frame");
  const second = part("next");
  const body = new ReadableStream<Uint8Array>({ start(controller) {
    controller.enqueue(first.slice(0, 15));
    controller.enqueue(first.slice(15, 80));
    const last = new Uint8Array(first.length - 80 + second.length);
    last.set(first.slice(80)); last.set(second, first.length - 80);
    controller.enqueue(last); controller.close();
  }});
  const frames = browserFrames(body);
  expect(new TextDecoder().decode((await frames.next()).value!)).toBe("jpeg\r\n\r\n--kageha-frame");
  expect(new TextDecoder().decode((await frames.next()).value!)).toBe("next");
  await expect(frames.next()).rejects.toThrow("connection ended");
});

it.each(["-1", "0", "999999999"])("rejects invalid frame lengths (%s) before allocating their body", async length => {
  const frames = browserFrames(new ReadableStream({ start(controller) {
    controller.enqueue(text(`--frame\r\nContent-Length: ${length}\r\n\r\n`));
  }}));
  await expect(frames.next()).rejects.toThrow("frame length");
});
