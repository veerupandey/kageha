import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { AskHumanPrompt } from "./AskHumanPrompt";

describe("AskHumanPrompt", () => {
  it("requires explicit confirmation before sending a chip answer", () => {
    const onAnswer = vi.fn();
    render(
      <AskHumanPrompt
        pending={{
          approval_id: "question:1",
          question: "Which loss term?",
          options: ["SupCon", "GenDice"],
        }}
        onAnswer={onAnswer}
        onDefer={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /GenDice/i }));
    expect(onAnswer).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
    expect(onAnswer).toHaveBeenCalledWith("GenDice");
  });

  it("prefers typed text over a selected chip", () => {
    const onAnswer = vi.fn();
    render(
      <AskHumanPrompt
        pending={{
          approval_id: "question:2",
          question: "Which repo?",
          options: ["kageha", "loomable"],
        }}
        onAnswer={onAnswer}
        onDefer={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: /kageha/i }));
    fireEvent.click(screen.getByRole("radio", { name: "Your own answer" }));
    fireEvent.change(screen.getByPlaceholderText("Something else…"), {
      target: { value: "custom fork" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
    expect(onAnswer).toHaveBeenCalledWith("custom fork");
  });
});
