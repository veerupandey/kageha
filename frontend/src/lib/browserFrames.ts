/** Read bounded multipart JPEG parts without relying on browser MJPEG images. */
export async function* browserFrames(body: ReadableStream<Uint8Array>) {
  const reader = body.getReader();
  let buffer = new Uint8Array(0);
  let length: number | null = null;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) throw new Error("Browser preview connection ended");
      const joined = new Uint8Array(buffer.length + value.length);
      joined.set(buffer); joined.set(value, buffer.length); buffer = joined;
      while (true) {
        if (length === null) {
          let end = -1;
          for (let i = 0; i + 3 < buffer.length; i++) {
            if (buffer[i] === 13 && buffer[i + 1] === 10 && buffer[i + 2] === 13 && buffer[i + 3] === 10) { end = i; break; }
          }
          if (end < 0) {
            if (buffer.length > 8192) throw new Error("Invalid browser preview headers");
            break;
          }
          const header = new TextDecoder().decode(buffer.subarray(0, end));
          const match = /(?:^|\r\n)Content-Length:\s*(\d+)\s*(?:\r\n|$)/i.exec(header);
          length = match ? Number(match[1]) : 0;
          if (length < 1 || length > 16 * 1024 * 1024) throw new Error("Invalid browser preview frame length");
          buffer = buffer.slice(end + 4);
        }
        if (buffer.length < length) break;
        const frame = buffer.slice(0, length);
        buffer = buffer.slice(length); length = null;
        yield frame;
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
