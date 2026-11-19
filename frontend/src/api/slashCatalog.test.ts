import { describe, expect, it } from "vitest";
import {
  filterSlashByCapabilities,
  mergeServerCatalog,
  SLASH_COMMANDS,
} from "./slashCatalog";
import type { SlashCommand, WebUiCapabilities } from "./types";

const caps = (
  partial: Partial<WebUiCapabilities> = {},
): WebUiCapabilities => ({
  projectFiles: null,
  cometApi: null,
  browserApi: null,
  computerApi: null,
  machineApi: null,
  modelApi: null,
  slashCatalogApi: false,
  ...partial,
});

describe("mergeServerCatalog", () => {
  it("returns fallback when server list empty", () => {
    expect(mergeServerCatalog([])).toEqual(SLASH_COMMANDS);
    expect(mergeServerCatalog(null)).toEqual(SLASH_COMMANDS);
  });

  it("treats a non-empty server catalog as authoritative", () => {
    const server = [
      {
        id: "plan",
        label: "/plan",
        description: "Server plan",
        kind: "mode",
        title: "Plan+",
      },
    ];
    const merged = mergeServerCatalog(server, SLASH_COMMANDS);
    const plan = merged.find((c) => c.id === "plan");
    expect(plan?.description).toBe("Server plan");
    expect(plan?.title).toBe("Plan+");
    expect(merged).toHaveLength(1);
  });

  it("does not retain stale fallback commands", () => {
    const server = [
      { id: "custom", label: "/custom", description: "Custom", kind: "prefs" },
    ];
    const merged = mergeServerCatalog(server, [
      {
        id: "ask",
        label: "/ask",
        description: "Ask",
        kind: "prefs",
      },
    ]);
    expect(merged.map((c) => c.id)).toEqual(["custom"]);
  });

  it("hides the internal /computer_use skill duplicate", () => {
    const server = [
      {
        id: "computer",
        label: "/computer",
        description: "Drive this Mac",
        kind: "skill",
        title: "computer_use",
      },
      {
        id: "skill-computer_use",
        label: "/computer_use",
        description: "duplicate",
        kind: "skill",
        title: "computer_use",
      },
      { id: "browser", label: "/browser", description: "Web", kind: "browser" },
      { id: "machine", label: "/machine", description: "Box", kind: "machine" },
    ];
    const merged = mergeServerCatalog(server, SLASH_COMMANDS);
    expect(merged.map((c) => c.id)).toEqual(["computer", "browser", "machine"]);
  });
});

describe("filterSlashByCapabilities", () => {
  const browserCmd: SlashCommand = {
    id: "browser",
    label: "/browser",
    description: "Browser",
    kind: "browser",
  };
  const computerCmd: SlashCommand = {
    id: "computer",
    label: "/computer",
    description: "Computer",
    kind: "computer",
  };
  const askCmd: SlashCommand = {
    id: "ask",
    label: "/ask",
    description: "Ask",
    kind: "prefs",
  };

  it("keeps browser when capability is null (unknown)", () => {
    const out = filterSlashByCapabilities(
      [browserCmd, askCmd],
      caps({ browserApi: null }),
    );
    expect(out.some((c) => c.id === "browser")).toBe(true);
  });

  it("hides browser when capability is explicitly false", () => {
    const out = filterSlashByCapabilities(
      [browserCmd, askCmd],
      caps({ browserApi: false }),
    );
    expect(out.some((c) => c.id === "browser")).toBe(false);
    expect(out.some((c) => c.id === "ask")).toBe(true);
  });

  it("hides computer when capability is false", () => {
    const out = filterSlashByCapabilities(
      [computerCmd, askCmd],
      caps({ computerApi: false }),
    );
    expect(out.some((c) => c.id === "computer")).toBe(false);
  });

  it("hides machine when capability is false", () => {
    const machineCmd: SlashCommand = {
      id: "machine",
      label: "/machine",
      description: "Machine",
      kind: "machine",
    };
    const out = filterSlashByCapabilities(
      [machineCmd, askCmd],
      caps({ machineApi: false }),
    );
    expect(out.some((c) => c.id === "machine")).toBe(false);
    expect(out.some((c) => c.id === "ask")).toBe(true);
  });
});
