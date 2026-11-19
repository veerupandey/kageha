import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { CredentialPrompt } from "./CredentialPrompt";

const pending = {
  approval_id: "credential:turn-1",
  action: "request_credential",
  risk_class: "hitl",
  secret: true,
  label: "instagram password",
} as const;

describe("CredentialPrompt", () => {
  afterEach(cleanup);

  it("renders a masked password input, never a plain text field", () => {
    render(<CredentialPrompt pending={pending} onSubmit={() => {}} onCancel={() => {}} />);
    const input = screen.getByPlaceholderText(/instagram password/i) as HTMLInputElement;
    expect(input.type).toBe("password");
    expect(input.autocomplete).toBe("off");
  });

  it("shows the label but no secret, and a security note", () => {
    render(<CredentialPrompt pending={pending} onSubmit={() => {}} onCancel={() => {}} />);
    expect(screen.getByText(/Enter your instagram password/i)).toBeInTheDocument();
    expect(screen.getByText(/never shown to the/i)).toBeInTheDocument();
  });

  it("submits the typed value via onSubmit and clears the field", () => {
    const onSubmit = vi.fn();
    render(<CredentialPrompt pending={pending} onSubmit={onSubmit} onCancel={() => {}} />);
    const input = screen.getByPlaceholderText(/instagram password/i) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "hunter2" } });
    fireEvent.click(screen.getByRole("button", { name: /send securely/i }));
    expect(onSubmit).toHaveBeenCalledWith("hunter2");
    expect(input.value).toBe(""); // secret dropped from component state
  });

  it("submits on Enter and cancels on Escape", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<CredentialPrompt pending={pending} onSubmit={onSubmit} onCancel={onCancel} />);
    const input = screen.getByPlaceholderText(/instagram password/i);
    fireEvent.change(input, { target: { value: "sekret" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith("sekret");

    cleanup();
    render(<CredentialPrompt pending={pending} onSubmit={onSubmit} onCancel={onCancel} />);
    fireEvent.keyDown(screen.getByPlaceholderText(/instagram password/i), { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });

  it("does not submit an empty value", () => {
    const onSubmit = vi.fn();
    render(<CredentialPrompt pending={pending} onSubmit={onSubmit} onCancel={() => {}} />);
    const btn = screen.getByRole("button", { name: /send securely/i }) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    fireEvent.click(btn);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("fires onSubmit at most once (double-click guard)", () => {
    const onSubmit = vi.fn();
    render(<CredentialPrompt pending={pending} onSubmit={onSubmit} onCancel={() => {}} />);
    const input = screen.getByPlaceholderText(/instagram password/i);
    fireEvent.change(input, { target: { value: "x" } });
    const btn = screen.getByRole("button", { name: /send securely/i });
    fireEvent.click(btn);
    fireEvent.click(btn);
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });
});
