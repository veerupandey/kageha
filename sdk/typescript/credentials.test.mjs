import test from "node:test";
import assert from "node:assert/strict";
import {
  CredentialClient,
  SecretLease,
  httpsCredentialTransport,
} from "./credentials.mjs";
test("credential values are explicit and redacted from ordinary inspection", () => {
  const lease = new SecretLease(
    new TextEncoder().encode("CANARY"),
    { reference: { namespace: "models", id: "a" } },
    Date.now() + 1000,
  );
  assert(!JSON.stringify(lease).includes("CANARY"));
  const value = lease.expose();
  lease.dispose();
  assert(value.every((v) => v === 0));
  assert.throws(() => lease.expose());
});
test("client never sends caller identity and preserves operation IDs", async () => {
  const calls = [];
  const c = new CredentialClient(async (path, body) => {
    calls.push({ path, body });
    return { operation_id: body.operation_id };
  });
  await c.put(
    { namespace: "models", id: "a" },
    {
      principal: "injected",
      purpose: "model",
      destination: "https://provider.example",
    },
    {
      operationId: "same",
      expectedRevision: 0,
      kind: "api_key",
      value: new TextEncoder().encode("fixture"),
    },
  );
  assert.equal(calls[0].body.operation_id, "same");
  assert.equal(calls[0].body.principal, undefined);
  assert.equal(calls.length, 1);
});
test("network transport requires explicit mutually authenticated client", () => {
  assert.throws(() => httpsCredentialTransport("http://localhost", fetch));
  assert.throws(() => httpsCredentialTransport("https://example.test", null));
});
