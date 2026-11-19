import { describe, expect, it } from "vitest";
import { diagramAnnotations, diagramEdges } from "./diagramDetails";

describe("diagram inspection data", () => {
  it("reads optional node evidence and ignores malformed annotations", () => {
    const details = diagramAnnotations(`flowchart LR
%% @node API {"description":"Accepts requests","files":["src/api.ts:12", 4]}
%% @node Broken {invalid JSON}
%% @node Wrong []
API --> DB`);
    expect(details.get("API")).toEqual({ description: "Accepts requests", files: ["src/api.ts:12"] });
    expect(details.size).toBe(1);
  });

  it("reads exact model endpoints including custom edge IDs and underscores", () => {
    const edges = diagramEdges({ getEdges: () => [
      { id: "custom", start: "api_v2", end: "data_store", text: "SQL<br/>requests" },
      { start: 1, end: null },
    ] });
    expect(edges).toEqual([{ id: "custom", start: "api_v2", end: "data_store", label: "SQL requests" }]);
    expect(diagramEdges({})).toEqual([]);
  });
});
