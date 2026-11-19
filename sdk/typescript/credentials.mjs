// Trusted workload client. Do not expose its transport or resolve permission to an agent or browser UI.
export class SecretLease {
  #value;
  constructor(value, metadata, validUntilMs) {
    this.#value = value;
    this.metadata = metadata;
    this.validUntilMs = validUntilMs;
  }
  expose() {
    if (!this.#value) throw Error("Credential lease is disposed");
    return this.#value;
  }
  dispose() {
    this.#value?.fill(0);
    this.#value = null;
  }
  toJSON() {
    return {
      metadata: this.metadata,
      validUntilMs: this.validUntilMs,
      secret: "[REDACTED]",
    };
  }
  [Symbol.for("nodejs.util.inspect.custom")]() {
    return this.toJSON();
  }
}
const query = (reference, access) => ({
  reference,
  purpose: access.purpose,
  destination: access.destination,
});
const encode = (value) => Buffer.from(value).toString("base64");
export class CredentialClient {
  constructor(transport) {
    if (typeof transport !== "function")
      throw TypeError("Authenticated credential transport required");
    this.transport = transport;
  }
  metadata(reference, access) {
    return this.transport("/v1/metadata", query(reference, access));
  }
  async resolve(reference, access) {
    const wire = await this.transport("/v1/resolve", query(reference, access));
    if (
      wire.metadata?.reference?.namespace !== reference.namespace ||
      wire.metadata?.reference?.id !== reference.id ||
      !Number.isSafeInteger(wire.valid_until_ms) ||
      wire.valid_until_ms <= Date.now() ||
      wire.valid_until_ms > Date.now() + 60000
    )
      throw Error("Invalid credential lease");
    if (
      typeof wire.secret_base64 !== "string" ||
      !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
        wire.secret_base64,
      )
    )
      throw Error("Invalid credential encoding");
    const value = new Uint8Array(Buffer.from(wire.secret_base64, "base64"));
    wire.secret_base64 = "";
    return new SecretLease(value, wire.metadata, wire.valid_until_ms);
  }
  async validate(lease, access) {
    await this.transport("/v1/check", {
      ...query(lease.metadata.reference, access),
      metadata: lease.metadata,
      valid_until_ms: lease.validUntilMs,
    });
  }
  put(
    reference,
    access,
    { operationId, expectedRevision, kind, value, expiresAtMs = null },
  ) {
    return this.transport("/v1/put", {
      ...query(reference, access),
      operation_id: operationId,
      expected_revision: expectedRevision,
      kind,
      expires_at_ms: expiresAtMs,
      secret_base64: encode(value),
    });
  }
  revoke(
    reference,
    access,
    { operationId, expectedRevision, deleteRecord = false },
  ) {
    return this.transport("/v1/revoke", {
      ...query(reference, access),
      operation_id: operationId,
      expected_revision: expectedRevision,
      delete: deleteRecord,
    });
  }
  operation(reference, access, operationId) {
    return this.transport("/v1/operation", {
      ...query(reference, access),
      operation_id: operationId,
    });
  }
  async beginRefresh(reference, access, expectedRevision, operationId) {
    const reply = await this.transport("/v1/oauth", {
      query: query(reference, access),
      call: {
        action: "begin",
        expected_revision: expectedRevision,
        operation_id: operationId,
      },
    });
    return reply.ticket;
  }
  async dispatchRefresh(ticket, access, operationId) {
    const reply = await this.transport("/v1/oauth", {
      query: query(ticket.reference, access),
      call: { action: "dispatch", ticket, operation_id: operationId },
    });
    const grant = new SecretLease(
      new Uint8Array(Buffer.from(reply.grant_base64, "base64")),
      {
        reference: ticket.reference,
        kind: "oauth_grant",
        revision: reply.ticket.revision,
        authority_generation: reply.ticket.authority_generation,
        state: "refresh_dispatching",
        expires_at_ms: reply.ticket.expires_at_ms,
      },
      reply.ticket.expires_at_ms,
    );
    reply.grant_base64 = "";
    return { ticket: reply.ticket, grant };
  }
  async completeRefresh(
    ticket,
    access,
    operationId,
    grant,
    expiresAtMs = null,
  ) {
    const reply = await this.transport("/v1/oauth", {
      query: query(ticket.reference, access),
      call: {
        action: "complete",
        ticket,
        operation_id: operationId,
        grant_base64: encode(grant),
        expires_at_ms: expiresAtMs,
      },
    });
    return reply.operation;
  }
  async recoverRefresh(reference, access, expectedRevision, operationId) {
    const reply = await this.transport("/v1/oauth", {
      query: query(reference, access),
      call: {
        action: "recover",
        expected_revision: expectedRevision,
        operation_id: operationId,
      },
    });
    return reply.operation;
  }
  failRefresh(ticket, access, operationId) {
    return this.transport("/v1/control", {
      query: query(ticket.reference, access),
      call: { action: "fail_refresh", ticket, operation_id: operationId },
    });
  }
  audits(namespace, access, after = 0) {
    return this.transport("/v1/audit", { namespace, ...access, after });
  }
  replacement(access, action) {
    return this.transport("/v1/replacements", {
      ...action,
      purpose: access.purpose,
      destination: access.destination,
    });
  }
}
export async function unixCredentialTransport({
  socket,
  tokenFile,
  timeoutMs = 25000,
}) {
  const http = await import("node:http");
  const fs = await import("node:fs/promises");
  const { constants } = await import("node:fs");
  return async (route, body) => {
    if (!/^\/v1\/[a-z]+$/.test(route))
      throw Error("Invalid credential operation");
    const file = await fs.open(
      tokenFile,
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
    let token;
    try {
      const stat = await file.stat();
      if (
        !stat.isFile() ||
        stat.uid !== process.getuid() ||
        (stat.mode & 0o077) !== 0 ||
        stat.size > 4096
      )
        throw Error("Credential token must be a private bounded regular file");
      token = await file.readFile();
    } finally {
      await file.close();
    }
    try {
      return await new Promise((resolve, reject) => {
        const req = http.request(
          {
            socketPath: socket,
            path: route,
            method: "POST",
            headers: {
              authorization: "Bearer " + token.toString("utf8").trim(),
              "content-type": "application/json",
            },
            timeout: timeoutMs,
          },
          (res) => {
            const chunks = [];
            let size = 0;
            res.on("data", (chunk) => {
              size += chunk.length;
              if (size > 256 * 1024) {
                res.destroy();
                reject(Error("Credential response exceeds limit"));
              } else chunks.push(chunk);
            });
            res.on("error", () =>
              reject(
                Error(
                  "Credential response interrupted; inspect operation status before retrying a mutation",
                ),
              ),
            );
            res.on("end", () => {
              if (res.statusCode < 200 || res.statusCode >= 300) {
                reject(
                  Error(
                    "Credential operation rejected (HTTP " +
                      res.statusCode +
                      ")",
                  ),
                );
                return;
              }
              if (res.statusCode === 204) {
                resolve(null);
                return;
              }
              try {
                resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
              } catch {
                reject(Error("Invalid credential response"));
              }
            });
          },
        );
        req.on("timeout", () => req.destroy());
        req.on("error", () =>
          reject(
            Error(
              "Credential transport failed; inspect operation status before retrying a mutation",
            ),
          ),
        );
        req.end(JSON.stringify(body));
      });
    } finally {
      token.fill(0);
    }
  };
}
export function httpsCredentialTransport(endpoint, authenticatedFetch) {
  const url = new URL(endpoint);
  if (
    url.protocol !== "https:" ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    typeof authenticatedFetch !== "function"
  )
    throw Error("An HTTPS endpoint and mTLS-configured fetch are required");
  return async (route, body) => {
    if (!/^\/v1\/[a-z]+$/.test(route))
      throw Error("Invalid credential operation");
    const response = await authenticatedFetch(
      endpoint.replace(/\/$/, "") + route,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
        redirect: "error",
        signal: AbortSignal.timeout(25000),
      },
    );
    if (!response.ok)
      throw Error(
        "Credential operation rejected (HTTP " + response.status + ")",
      );
    if (response.status === 204) return null;
    const reader = response.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > 256 * 1024) throw Error("Credential response exceeds limit");
        chunks.push(value);
      }
    } finally {
      await reader.cancel();
    }
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  };
}
