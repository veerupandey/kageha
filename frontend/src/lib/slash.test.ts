import { describe, expect, it } from "vitest";
import {
  applySlashCommand,
  filterSlashCommands,
  getAtContext,
  getSlashContext,
  groupSlashItems,
  slashCommandGroup,
  slashCommandTitle,
  slashCommandToken,
  slashCommandUsage,
} from "./slash";
import { SLASH_COMMANDS } from "../api/slashCatalog";
import { useAppStore } from "../store";
import {
  isComputerAdminSlash,
  isMachineAdminSlash,
  isMetaOnlySlash,
  isModeOnlyComposerText,
} from "../store/helpers";

describe("getSlashContext", () => {
  it("detects slash token at caret", () => {
    const text = "hello /pl";
    const ctx = getSlashContext(text, text.length);
    expect(ctx).toEqual({
      start: 6,
      end: 9,
      token: "/pl",
      query: "pl",
    });
  });

  it("returns null when not in a slash token", () => {
    expect(getSlashContext("hello world", 5)).toBeNull();
    expect(getSlashContext("email@x.com", 11)).toBeNull();
  });

  it("allows slash at start of input", () => {
    const ctx = getSlashContext("/plan", 5);
    expect(ctx?.token).toBe("/plan");
    expect(ctx?.query).toBe("plan");
    expect(ctx?.start).toBe(0);
  });
});

describe("getAtContext", () => {
  it("detects @token at caret", () => {
    const text = "see @file";
    const ctx = getAtContext(text, text.length);
    expect(ctx).toEqual({
      start: 4,
      end: 9,
      token: "@file",
      query: "file",
    });
  });

  it("returns null outside @token", () => {
    expect(getAtContext("plain text", 4)).toBeNull();
  });
});

describe("slash command presentation", () => {
  it("shows action groups and argument syntax", () => {
    const command = {
      id: "browser-diagnose",
      label: "/browser diagnose",
      description: "Inspect a page",
      kind: "browser",
    };
    expect(slashCommandGroup(command)).toBe("Browser & research");
    expect(slashCommandUsage(command)).toBe("/browser diagnose <url>");
  });

  it("keeps /computer as Computer, not mixed with browser or machine", () => {
    const computer = {
      id: "computer",
      label: "/computer",
      title: "computer_use",
      description: "Drive this Mac",
      kind: "skill",
      usage: "/computer <task>",
    };
    expect(slashCommandTitle(computer)).toBe("Computer");
    expect(slashCommandUsage(computer)).toBe("/computer <task>");
    expect(slashCommandGroup(computer)).toBe("Skills");
    expect(slashCommandGroup({ ...computer, kind: "computer" })).toBe(
      "Computer use",
    );
    expect(
      slashCommandGroup({
        id: "browser",
        label: "/browser",
        description: "",
        kind: "browser",
      }),
    ).toBe("Browser & research");
    expect(
      slashCommandGroup({
        id: "machine",
        label: "/machine",
        description: "",
        kind: "machine",
      }),
    ).toBe("Task machine");
  });

  it("includes /computer status in the complete root slash menu", () => {
    const cmds = [
      {
        id: "computer",
        label: "/computer",
        title: "computer_use",
        description: "Drive this Mac",
        kind: "skill",
      },
      {
        id: "computer-status",
        label: "/computer status",
        description: "Driver, permissions, pack",
        kind: "computer",
      },
      {
        id: "browser",
        label: "/browser",
        description: "Web",
        kind: "browser",
      },
    ];
    expect(filterSlashCommands(cmds, "").map((c) => c.id)).toEqual([
      "browser",
      "computer",
      "computer-status",
    ]);
    expect(filterSlashCommands(cmds, "computer").map((c) => c.id)).toEqual([
      "computer",
      "computer-status",
    ]);
  });

  it("ranks Plan first when the user types /p", () => {
    const ids = filterSlashCommands(SLASH_COMMANDS, "p").map((cmd) => cmd.id);
    expect(ids[0]).toBe("plan");
    expect(ids).toContain("permissions");
  });

  it("lists grouped commands for a bare slash", () => {
    const items = filterSlashCommands(SLASH_COMMANDS, "");
    expect(items.some((cmd) => cmd.id === "plan")).toBe(true);
    expect(items.some((cmd) => cmd.id === "computer-status")).toBe(true);
    const groups = groupSlashItems(items).map((row) => row.group);
    expect(groups[0]).toBe("Modes");
  });

  it("uses the slash label as the picker token", () => {
    const plan = SLASH_COMMANDS.find((cmd) => cmd.id === "plan");
    expect(plan).toBeTruthy();
    expect(slashCommandToken(plan!)).toBe("/plan");
  });
});

describe("applySlashCommand", () => {
  it("turns Plan into a composer chip and clears the slash token", () => {
    useAppStore.setState({
      draft: "/p",
      agentMode: "normal",
      composerChip: { kind: null, value: null },
    });
    const plan = SLASH_COMMANDS.find((cmd) => cmd.id === "plan")!;
    applySlashCommand(plan, { start: 0, end: 2 });
    const state = useAppStore.getState();
    expect(state.agentMode).toBe("plan");
    expect(state.composerChip).toEqual({ kind: "mode", value: "plan" });
    expect(state.draft).toBe("");
  });
});

describe("meta-only slash (must not mint sessions)", () => {
  it("treats mode-only and bare skill prefixes as meta", () => {
    expect(isModeOnlyComposerText("/plan")).toBe(true);
    expect(isMetaOnlySlash("/plan")).toBe(true);
    expect(isMetaOnlySlash("/computer")).toBe(true);
    expect(isMetaOnlySlash("/computer_use")).toBe(true);
    expect(isComputerAdminSlash("/computer status")).toBe(true);
    expect(isMetaOnlySlash("/computer status")).toBe(true);
    expect(isMachineAdminSlash("/machine")).toBe(true);
    expect(isMachineAdminSlash("/machine status")).toBe(true);
    expect(isMetaOnlySlash("/machine stop")).toBe(true);
  });

  it("lets skill tasks through to the agent", () => {
    expect(isComputerAdminSlash("/computer open Calculator")).toBe(false);
    expect(isMetaOnlySlash("/computer open Calculator")).toBe(false);
    expect(isMetaOnlySlash("/computer_use open Chrome")).toBe(false);
    expect(isMachineAdminSlash("/machine install deps")).toBe(false);
    expect(isMetaOnlySlash("/machine install deps")).toBe(false);
  });
});


describe("catalog command contexts", () => {
  it("completes underscored skills", () => {
    expect(getSlashContext("/my_skill", 9)?.query).toBe("my_skill");
  });
  it("completes subcommands without treating task arguments as commands", () => {
    expect(getSlashContext("/browser ch", 11, SLASH_COMMANDS)?.query).toBe("browser ch");
    expect(getSlashContext("/permissions ", 13, SLASH_COMMANDS)?.query).toBe("permissions ");
    expect(getSlashContext("/research explain gravity", 25, SLASH_COMMANDS)).toBeNull();
  });
  it("recognizes team sigils without matching inline email addresses", () => {
    expect(getAtContext("&found", 6, "&")?.query).toBe("found");
    expect(getAtContext("a@b.com", 7)).toBeNull();
    expect(getAtContext("a&b", 3, "&")).toBeNull();
  });
});
