import { describe, expect, it } from "vitest";
import { credentialIntent } from "./askHumanApproval";

describe("credentialIntent (UI safety net)", () => {
  it("flags secret solicitations as credential", () => {
    for (const q of [
      "What is the LinkedIn password for that account?",
      "enter your api key",
      "share the secret key",
      "what is your passphrase?",
      "provide the login credential",
      "type your PIN",
    ]) {
      expect(credentialIntent(q)).toBe("credential");
    }
  });

  it("flags OTP / 2FA code requests as verification", () => {
    for (const q of [
      "read me the code from the SMS",
      "enter the OTP",
      "what is the 2FA code",
      "the verification code sent to your phone",
      "give me the sign-in code from the app",
    ]) {
      expect(credentialIntent(q)).toBe("verification");
    }
  });

  it("leaves ordinary questions alone", () => {
    for (const q of [
      "Which marketplace should I use?",
      "What layout do you prefer?",
      "Pick a filename",
      "Which loss should we prioritize?",
    ]) {
      expect(credentialIntent(q)).toBe("");
    }
  });
});
