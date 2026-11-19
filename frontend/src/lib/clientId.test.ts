import { afterEach, describe, expect, it, vi } from "vitest";
import { createClientId } from "./clientId";

const supportedCrypto = globalThis.crypto;
afterEach(() => vi.unstubAllGlobals());

describe("client request identity", () => {
  it("uses native UUID generation with the proper receiver when supported", () => {
    const source = { randomUUID: vi.fn(function (this: unknown) {
      expect(this).toBe(source);
      return "9fd5faeb-0adb-4d52-a77f-b45c4da138c0";
    }) };
    vi.stubGlobal("crypto", source);
    expect(createClientId()).toBe("9fd5faeb-0adb-4d52-a77f-b45c4da138c0");
    expect(source.randomUUID).toHaveBeenCalledOnce();
  });

  it("generates distinct UUID v4 identities without randomUUID on LAN HTTP", () => {
    vi.stubGlobal("crypto", { getRandomValues: supportedCrypto.getRandomValues.bind(supportedCrypto) });
    const ids = Array.from({ length: 100 }, createClientId);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) {
      expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    }
  });

  it("does not substitute weak randomness when Web Crypto is entirely absent", () => {
    vi.stubGlobal("crypto", undefined);
    expect(createClientId).toThrow("cannot generate secure request IDs");
  });
});
