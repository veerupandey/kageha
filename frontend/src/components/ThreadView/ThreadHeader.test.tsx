import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { ThreadHeader } from "./ThreadHeader";

const counts = {
  all: 0,
  images: 0,
  video: 0,
  code: 0,
  webpages: 0,
  documents: 0,
};

vi.mock("../../lib/sessionTruth", () => ({ useSessionTruth: () => ({ data: null, status: "checking", checkedAt: 0 }) }));

let mockStoreState: Record<string, unknown> = {};

vi.mock("../../store", () => ({
  useAppStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector(mockStoreState),
}));

describe("ThreadHeader", () => {
  beforeEach(() => {
    mockStoreState = {
      sessionTitle: "Test thread",
      sessionId: "abc12345",
      runStatus: "idle",
      modelOverride: "",
      defaultModel: "glm-5.3-flash",
      modelCatalog: [],
      prefs: { rightPanelCollapsed: true, mainModel: "glm-5.3-flash" },
      capabilities: { computerApi: false, browserApi: false },
      machineActive: false,
      messages: [],
      workspaceSurface: "artifacts",
      liveSurface: null,
      sessions: [],
      projects: [],
      sessionProject: null,
      loadProjects: vi.fn(),
      setSessionProject: vi.fn(),
      setPrefs: vi.fn(),
      setComputerPanelMode: vi.fn(),
      setBrowserPanelMode: vi.fn(),
      pinLiveSurface: vi.fn(),
      pinWorkspaceSurface: vi.fn(),
      renameSession: vi.fn(),
      showToast: vi.fn(),
    };
  });

  describe("mobile menu", () => {
    it("renders sidebar opener and calls handler on tap", () => {
      const onOpenSidebar = vi.fn();
      render(
        <ThreadHeader
          artifactFilter="all"
          onFilterChange={() => {}}
          artifactCounts={counts}
          onOpenSidebar={onOpenSidebar}
        />,
      );
      const menu = screen.getByRole("button", { name: "Open menu" });
      fireEvent.click(menu);
      expect(onOpenSidebar).toHaveBeenCalledTimes(1);
    });
  });

  describe("project display", () => {
    it("renders + Project when no project is assigned", () => {
      render(
        <ThreadHeader
          artifactFilter="all"
          onFilterChange={() => {}}
          artifactCounts={counts}
        />,
      );
      expect(screen.getByText("+ Project")).toBeInTheDocument();
    });

    it("renders assigned project name on top in heading in place of + Project", () => {
      mockStoreState.sessionProject = {
        id: "bare-fair",
        name: "Bare & Fair",
        color: "emerald",
      };
      render(
        <ThreadHeader
          artifactFilter="all"
          onFilterChange={() => {}}
          artifactCounts={counts}
        />,
      );
      expect(screen.getByText("Bare & Fair")).toBeInTheDocument();
      expect(screen.queryByText("+ Project")).not.toBeInTheDocument();
    });

    it("renders assigned project from active session summary if sessionProject is null", () => {
      mockStoreState.sessions = [
        {
          session_id: "abc12345",
          project_id: "bare-fair",
          project: {
            id: "bare-fair",
            name: "Bare & Fair",
            color: "emerald",
          },
        },
      ];
      render(
        <ThreadHeader
          artifactFilter="all"
          onFilterChange={() => {}}
          artifactCounts={counts}
        />,
      );
      expect(screen.getByText("Bare & Fair")).toBeInTheDocument();
      expect(screen.queryByText("+ Project")).not.toBeInTheDocument();
    });
  });
});
