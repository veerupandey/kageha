import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AgentDisclosure } from "./AgentDisclosure";
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
it("restores open details only for the selected specialist", async () => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value) });
  const first = render(<AgentDisclosure scope="chat:mira" name="assignment"><summary>Assignment</summary><p>Details</p></AgentDisclosure>);
  const details = screen.getByText("Assignment").closest("details")!;
  details.open = true; fireEvent(details, new Event("toggle"));
  await waitFor(() => expect([...data.values()]).toContain("true"));
  first.unmount();
  const second = render(<AgentDisclosure scope="chat:mira" name="assignment"><summary>Assignment</summary><p>Details</p></AgentDisclosure>);
  expect(screen.getByText("Assignment").closest("details")).toHaveAttribute("open");
  second.unmount();
  render(<AgentDisclosure scope="chat:niko" name="assignment"><summary>Assignment</summary><p>Details</p></AgentDisclosure>);
  expect(screen.getByText("Assignment").closest("details")).not.toHaveAttribute("open");
});
