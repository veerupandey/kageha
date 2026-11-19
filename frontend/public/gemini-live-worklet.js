/**
 * AudioWorkletProcessor for Gemini Live mic capture.
 *
 * Runs on the dedicated audio rendering thread (unlike the deprecated
 * ScriptProcessorNode, which ran on the main thread and could glitch under
 * UI load). Just accumulates raw Float32 samples into fixed-size chunks and
 * posts them to the main thread — no downsampling or WebSocket I/O here;
 * AudioWorkletGlobalScope has no fetch/WebSocket access, so that stays in
 * geminiLiveClient.ts on the main thread.
 *
 * Loaded via `audioContext.audioWorklet.addModule("/gemini-live-worklet.js")`.
 * Must be a plain script (no ES module import/export) — Chrome/Firefox load
 * worklet modules in a restricted global scope.
 */
class GeminiLiveCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._chunkSize = 4096;
    this._buffer = new Float32Array(this._chunkSize);
    this._writeIndex = 0;
  }

  process(inputs) {
    const input = inputs[0];
    const channel = input && input[0];
    if (!channel || channel.length === 0) {
      return true;
    }
    for (let i = 0; i < channel.length; i++) {
      this._buffer[this._writeIndex++] = channel[i];
      if (this._writeIndex >= this._chunkSize) {
        // Transfer ownership of the underlying buffer to the main thread
        // (zero-copy) and allocate a fresh one to keep filling.
        this.port.postMessage(this._buffer, [this._buffer.buffer]);
        this._buffer = new Float32Array(this._chunkSize);
        this._writeIndex = 0;
      }
    }
    return true;
  }
}

registerProcessor("gemini-live-capture", GeminiLiveCaptureProcessor);
