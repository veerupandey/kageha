/** Operator-level memory client. Never pass this credential into an agent prompt. */
export class MemoryClient {
  constructor(endpoint, token, writer = null) {
    const url = new URL(endpoint);
    if (
      (url.protocol !== "https:" &&
        !(
          url.protocol === "http:" &&
          ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)
        )) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      token.length < 32
    )
      throw Error("HTTPS or loopback endpoint and operator token required");
    this.endpoint = endpoint.replace(/\/$/, "");
    this.token = token;
    this.writer = writer;
  }
  async request(path, call, signal) {
    const response = await fetch(this.endpoint + path, {
      method: call ? "POST" : "GET",
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(this.writer
          ? { "x-kageha-writer": JSON.stringify(this.writer) }
          : {}),
        ...(call ? { "Content-Type": "application/json" } : {}),
      },
      body: call ? JSON.stringify(call) : undefined,
      redirect: "error",
      signal: AbortSignal.any([
        AbortSignal.timeout(30000),
        ...(signal ? [signal] : []),
      ]),
    });
    const reader = response.body.getReader();
    const parts = [];
    let size = 0;
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > 32 * 1024 * 1024) throw Error("Response exceeds size limit");
        parts.push(value);
      }
    } finally {
      await reader.cancel();
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const part of parts) {
      bytes.set(part, offset);
      offset += part.length;
    }
    const result = JSON.parse(new TextDecoder().decode(bytes));
    if (!response.ok)
      throw Object.assign(
        Error(result.message || `Memory HTTP ${response.status}`),
        { code: result.code, retryable: result.retryable },
      );
    return result;
  }
  info(signal) {
    return this.request("/v1/info", undefined, signal);
  }
  withWriter(fence) {
    return new MemoryClient(this.endpoint, this.token, fence);
  }
  writerStatus(signal) {
    return this.request("/v1/fence", undefined, signal);
  }
  claimWriter(owner, expectedEpoch, requestId, signal) {
    return this.request(
      "/v1/fence",
      {
        operation: "claim",
        owner,
        expected_epoch: expectedEpoch,
        request_id: requestId,
      },
      signal,
    );
  }
  retireWriter(fence, signal) {
    return this.request("/v1/fence", { operation: "retire", fence }, signal);
  }
  call(operation, signal) {
    return this.request("/v1/calls", operation, signal);
  }
}

export { AgentClient, AgentError } from "./agent.mjs";
