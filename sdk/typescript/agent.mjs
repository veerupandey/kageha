/** Standalone runtime SDK. Operator credentials must stay outside model prompts. */
const LIMIT = 32 * 1024 * 1024;
const terminal = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
export class AgentError extends Error {
  constructor(
    message,
    { status = null, code = "sdk_protocol", retryable = false } = {},
  ) {
    super(message);
    this.name = "AgentError";
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}
function id(value) {
  if (
    typeof value !== "string" ||
    !value ||
    new TextEncoder().encode(value).length > 128 ||
    /[\x00-\x1f\x7f-\x9f]/.test(value) ||
    value === "." ||
    value === ".."
  )
    throw new AgentError("Invalid resource identity");
  return encodeURIComponent(value);
}
function cursor(value) {
  if (!Number.isSafeInteger(value) || value < 0)
    throw new AgentError("Event cursor must be a nonnegative safe integer");
  return value;
}
async function readJSON(response) {
  if (!response.body) throw new AgentError("Missing runtime response");
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > LIMIT)
        throw new AgentError("Runtime response exceeds size limit");
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  let result;
  try {
    result = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    );
  } catch {
    if (response.ok) throw new AgentError("Invalid runtime JSON response");
  }
  if (!response.ok)
    throw new AgentError(
      typeof result?.message === "string"
        ? result.message
        : "Runtime request rejected",
      {
        status: response.status,
        code: typeof result?.code === "string" ? result.code : "http_error",
        retryable: result?.retryable === true,
      },
    );
  return result;
}
export class AgentClient {
  #token;
  #endpoint;
  #timeoutMs;
  constructor(endpoint, token, { timeoutMs = 30000 } = {}) {
    const url = new URL(endpoint);
    if (
      !(
        url.protocol === "https:" ||
        (url.protocol === "http:" &&
          (["localhost", "[::1]"].includes(url.hostname) ||
            /^127\./.test(url.hostname)))
      ) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      typeof token !== "string" ||
      !/^[\x21-\x7e]{24,}$/.test(token)
    )
      throw new AgentError(
        "HTTPS or loopback endpoint and an operator token required",
      );
    if (
      !Number.isSafeInteger(timeoutMs) ||
      timeoutMs <= 0 ||
      timeoutMs > 2147483647
    )
      throw new AgentError("Positive timeout required");
    this.#endpoint = url.href.replace(/\/$/, "");
    this.#token = token;
    this.#timeoutMs = timeoutMs;
  }
  async request(path, body, signal) {
    try {
      return await readJSON(
        await fetch(this.#endpoint + path, {
          method: body === undefined ? "GET" : "POST",
          headers: {
            Authorization: `Bearer ${this.#token}`,
            ...(body === undefined
              ? {}
              : { "Content-Type": "application/json" }),
          },
          body: body === undefined ? undefined : JSON.stringify(body),
          redirect: "manual",
          signal: AbortSignal.any([
            AbortSignal.timeout(this.#timeoutMs),
            ...(signal ? [signal] : []),
          ]),
        }),
      );
    } catch (e) {
      if (e instanceof AgentError || signal?.aborted) throw e;
      throw new AgentError(
        "Runtime transport failed; request outcome may be unknown. Inspect status before retrying input.",
      );
    }
  }
  info(signal) {
    return this.request("/v1/info", undefined, signal);
  }
  run(spec, signal) {
    id(spec.id);
    return this.request("/v1/runs", spec, signal);
  }
  status(run, signal) {
    return this.request(`/v1/runs/${id(run)}`, undefined, signal);
  }
  steer(run, message, signal) {
    id(message.id);
    return this.request(`/v1/runs/${id(run)}/steer`, message, signal);
  }
  cancel(run, signal) {
    return this.request(`/v1/runs/${id(run)}/cancel`, {}, signal);
  }
  approvals(filter = {}, signal) {
    const query = new URLSearchParams();
    for (const key of ["session", "turn", "pending", "after"])
      if (filter[key] !== undefined) {
        if (key === "after") cursor(filter[key]);
        query.set(key, String(filter[key]));
      }
    return this.request(`/v1/approvals?${query}`, undefined, signal);
  }
  approval(approval, signal) {
    return this.request(`/v1/approvals/${id(approval)}`, undefined, signal);
  }
  decide(approval, decision, signal) {
    return this.request(
      `/v1/approvals/${id(approval)}/decision`,
      decision,
      signal,
    );
  }
  /** Explicit cursor resume; break/return closes the connection, not the server run. */
  async *events(run, { after = 0, signal } = {}) {
    const path = `/v1/runs/${id(run)}/events?after=${cursor(after)}`;
    const controller = new AbortController();
    const combined = AbortSignal.any([
      controller.signal,
      ...(signal ? [signal] : []),
    ]);
    let timer, reader;
    const timed = async (action) => {
      timer = setTimeout(() => controller.abort(), this.#timeoutMs);
      try {
        return await action();
      } finally {
        clearTimeout(timer);
      }
    };
    try {
      const response = await timed(() =>
        fetch(this.#endpoint + path, {
          headers: {
            Authorization: `Bearer ${this.#token}`,
            Accept: "text/event-stream",
          },
          redirect: "manual",
          signal: combined,
        }),
      );
      if (!response.ok) {
        await timed(() => readJSON(response));
      }
      if (
        response.headers.get("content-type")?.split(";")[0] !==
          "text/event-stream" ||
        !response.body
      )
        throw new AgentError("Expected runtime event stream");
      reader = response.body.getReader();
      const decoder = new TextDecoder("utf-8", { fatal: true });
      const encoder = new TextEncoder();
      let buffer = "",
        data = [],
        kind = "",
        eventId = "",
        size = 0;
      while (true) {
        const { done, value } = await timed(() => reader.read());
        if (value?.length > LIMIT)
          throw new AgentError("Runtime event chunk exceeds size limit");
        buffer += decoder.decode(value, { stream: !done });
        while (true) {
          const index = buffer.search(/[\r\n]/);
          if (
            index < 0 ||
            (buffer[index] === "\r" && index === buffer.length - 1 && !done)
          )
            break;
          const line = buffer.slice(0, index);
          const length =
            buffer[index] === "\r" && buffer[index + 1] === "\n" ? 2 : 1;
          buffer = buffer.slice(index + length);
          size += encoder.encode(line).length + length;
          if (size > LIMIT)
            throw new AgentError("Runtime event exceeds size limit");
          if (!line) {
            if (data.length) {
              if (kind !== "run")
                throw new AgentError(
                  "Runtime event stream reported an error or unknown event",
                );
              let event;
              try {
                event = JSON.parse(data.join("\n"));
              } catch {
                throw new AgentError("Invalid runtime event JSON");
              }
              cursor(event.sequence);
              if (
                String(event.sequence) !== eventId ||
                event.sequence <= after ||
                event.record?.spec?.id !== run
              )
                throw new AgentError(
                  "Out-of-order event or wrong run identity",
                );
              after = event.sequence;
              yield event;
              if (terminal.has(event.record.state)) return;
            }
            data = [];
            kind = "";
            eventId = "";
            size = 0;
          } else if (!line.startsWith(":")) {
            const colon = line.indexOf(":");
            const field = colon < 0 ? line : line.slice(0, colon);
            const v = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
            if (field === "data") data.push(v);
            else if (field === "event") kind = v;
            else if (field === "id") eventId = v;
          }
        }
        if (size + encoder.encode(buffer).length > LIMIT)
          throw new AgentError("Runtime event exceeds size limit");
        if (done) {
          if (buffer || data.length)
            throw new AgentError(
              "Truncated runtime event; resume from the last delivered cursor",
            );
          return;
        }
      }
    } catch (e) {
      if (e instanceof AgentError || signal?.aborted) throw e;
      throw new AgentError(
        "Runtime event transport failed; resume from the last delivered cursor",
      );
    } finally {
      clearTimeout(timer);
      controller.abort();
      if (reader) {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
    }
  }
}
