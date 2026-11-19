import { expect, it } from "vitest";
import type { ChatMessage } from "../api/types";
import { deliveredPaths, observedSources } from "./deliverables";

it("only delivers exact output references from a finished assistant answer", () => {
  const final: ChatMessage = {
    id: "a", role: "assistant", text: "See [report](artifacts/report.txt) and `uploads/resume.pdf`.",
    attachments: ["artifacts/report.txt", "artifacts/draft.txt", "uploads/resume.pdf"],
  };
  const paths = [...final.attachments!, "artifacts/report.txt.old"];
  expect(deliveredPaths(final, paths)).toEqual(["artifacts/report.txt"]);
  expect(deliveredPaths({ ...final, streaming: true }, paths)).toEqual([]);
  expect(deliveredPaths({ ...final, resultStatus: "failed" }, paths)).toEqual([]);
  expect(deliveredPaths({
    ...final, text: "Delivering [report](artifacts/final%20report.pdf) and [resume](uploads/resume.pdf).",
  }, [...paths, "artifacts/final report.pdf"])).toEqual(["uploads/resume.pdf", "artifacts/final report.pdf"]);
});

it("resolves workspace-relative output references to registered files", () => {
  const message: ChatMessage = {
    id: "a", role: "assistant", text: "Read `outputs/profile.md`.",
    attachments: ["workspace/outputs/profile.md"],
  };
  expect(deliveredPaths(message, ["workspace/outputs/profile.md"])).toEqual(["workspace/outputs/profile.md"]);
  expect(deliveredPaths({ ...message, text: "Read [profile](outputs/profile.md)." },
    ["workspace/outputs/profile.md"])).toEqual(["workspace/outputs/profile.md"]);
  expect(deliveredPaths({ ...message, text: "Read `outputs/other.md`." },
    ["workspace/outputs/profile.md"])).toEqual([]);
});

it("only shows cited successful reads from the same turn, including replayed tool blocks", () => {
  const observed = { url: "https://example.com/verified", title: "Verified source" };
  const failed = { url: "https://example.com/failed", title: "Failed source" };
  const final: ChatMessage = {
    id: "a", role: "assistant", text: `[Read](${observed.url}) [Failed](${failed.url}) https://example.com/unread`,
    blocks: [
      { id: "good", type: "tool", card: { id: "good", name: "web_fetch", status: "done", observedSource: observed } },
      { id: "bad", type: "tool", card: { id: "bad", name: "web_fetch", status: "failed", observedSource: failed } },
    ],
  };
  expect(observedSources(final)).toEqual([observed]);
});
