import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { IntegrationPanel } from "./IntegrationPanel";

it("starts collapsed and preserves entered configuration across toggles", () => {
  render(<IntegrationPanel name="Gmail" description="Email" connected icon={null}><input aria-label="Setup value" defaultValue="" /></IntegrationPanel>);
  const toggle = screen.getByRole("button", { name: /Gmail/ });
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(screen.getByLabelText("Setup value")).not.toBeVisible();
  fireEvent.click(toggle);
  fireEvent.change(screen.getByLabelText("Setup value"), { target: { value: "saved input" } });
  fireEvent.click(toggle);
  fireEvent.click(toggle);
  expect(screen.getByLabelText("Setup value")).toBeVisible();
  expect(screen.getByLabelText("Setup value")).toHaveValue("saved input");
});
