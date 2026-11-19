import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useAppStore } from "../store";
import { ApprovalBanner } from "./ApprovalBanner";
import { suppressResolvedApproval } from "../store/helpers";
import { AskHumanPrompt } from "./AskHumanPrompt";
import { AskHumanBlock } from "./Transcript/AskHumanBlock";

describe("ask_human UI", () => {
  afterEach(() => {
    cleanup();
    useAppStore.setState({ pendingApproval: null, messages: [] });
  });

  it("routes browser login approval to a confirmation card without secret inputs", () => {
    useAppStore.setState({
      sessionId: "login-chat",
      pendingApproval: { approval_id: "login-1", action: "request_secure_login", risk_class: "browser", detail: JSON.stringify({ website: "https://example.test", purpose: "Read orders" }) },
    });
    render(<ApprovalBanner />);
    expect(screen.getByText("https://example.test")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve access and resume" })).toBeDisabled();
    expect(document.querySelector('input[type="password"]')).toBeNull();
  });

  it("presents an upload destination and file with a one-use approval", () => {
    useAppStore.setState({
      sessionId: "upload-chat",
      pendingApproval: {
        approval_id: "upload-1",
        action: "browser_upload",
        risk_class: "browser",
        detail: "Attach these chat files to https://example.test in the private profile?\n• inputs/resume.pdf",
      },
    });
    render(<ApprovalBanner />);
    expect(screen.getByText(/https:\/\/example\.test.*inputs\/resume\.pdf/s)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Allow upload once" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "All routine permissions" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Full access" })).not.toBeInTheDocument();
  });

  it.each([
    ["host_import_files", "/Users/example/Downloads/clip.mov", "Allow these files"],
    ["computer_request_control", "Edit the document in this turn", "Allow control this turn"],
    ["computer_launch_app", "Open com.apple.Terminal", "Allow launch once"],
  ])("shows the scope of %s before granting access", (action, detail, label) => {
    useAppStore.setState({ sessionId: "host-chat", pendingApproval: { approval_id: action, action, detail } });
    render(<ApprovalBanner />);
    expect(screen.getByText(detail)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: label })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Full access" })).not.toBeInTheDocument();
  });

  it("presents a native dialog decision with a one-use approval", () => {
    useAppStore.setState({
      sessionId: "dialog-chat",
      pendingApproval: {
        approval_id: "dialog-1",
        action: "browser_dialog",
        risk_class: "browser",
        detail: 'Dismiss confirm on "https://example.test/page"?\nMessage "Continue?"',
      },
    });
    render(<ApprovalBanner />);
    expect(screen.getByText(/Dismiss confirm.*Continue\?/s)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Allow response once" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "All routine permissions" })).not.toBeInTheDocument();
  });

  it("does not show a browser approval that was already resolved", () => {
    const pending = { approval_id: "resolved-browser-banner", action: "request_secure_login", detail: "Continue LinkedIn" };
    suppressResolvedApproval("resolved-browser-chat", pending.approval_id);
    useAppStore.setState({ sessionId: "resolved-browser-chat", pendingApproval: pending });
    render(<ApprovalBanner />);
    expect(screen.queryByRole("button", { name: "Approve access and resume" })).not.toBeInTheDocument();
  });

  it("offers a one-time review for an earlier browser click", () => {
    useAppStore.setState({
      sessionId: "review-chat",
      pendingApproval: { approval_id: "review-1", action: "browser_review_action",
        risk_class: "browser", detail: "The sign-in form is visible." },
    });
    render(<ApprovalBanner />);
    expect(screen.getByRole("button", { name: "Continue without repeating" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "All routine permissions" })).not.toBeInTheDocument();
  });

  it("shows a fallback ask_human prompt when there is no assistant message yet", () => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "question:turn-1",
        sessionId: "session-1",
        action: "ask_human",
        risk_class: "hitl",
        question: "Which marketplace should I use?",
      },
      messages: [],
    });

    render(<ApprovalBanner />);

    expect(screen.getByText("Which marketplace should I use?")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send answer" })).toBeInTheDocument();
  });

  it("renders the shared prompt component with chips and a text field", () => {
    render(
      <AskHumanPrompt
        pending={{
          approval_id: "question:turn-2",
          question: "Which loss should we prioritize?",
          options: ["SupCon", "GenDice", "BCE"],
        }}
        onAnswer={() => {}}
        onDefer={() => {}}
      />,
    );

    expect(screen.getByText("Which loss should we prioritize?")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /SupCon/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Your own answer" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Skip" })).toBeInTheDocument();
  });

  it("still renders Continue / Stop for budget pauses", () => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "budget:turn-1",
        sessionId: "session-1",
        action: "continue_after_budget",
        risk_class: "loop",
        detail: "I've used the automatic budget bumps (5×).",
      },
    });

    render(<ApprovalBanner />);

    expect(screen.getByText("Keep going?")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Continue" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
  });

  it("routes a credential request to the masked CredentialPrompt, not ask_human", () => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "credential:turn-1",
        sessionId: "session-1",
        action: "request_credential",
        risk_class: "hitl",
        secret: true,
        label: "instagram password",
      },
      messages: [],
    });

    render(<ApprovalBanner />);

    // Masked prompt, not the plain-text answer field.
    const input = screen.getByPlaceholderText(/instagram password/i) as HTMLInputElement;
    expect(input.type).toBe("password");
    expect(screen.getByRole("button", { name: /send securely/i })).toBeInTheDocument();
    // The ask_human "Send answer" button must NOT be present.
    expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
  });

  it.each(["credential:turn-1", "question:older-turn"])("keeps exactly one working prompt with transcript approval %s", async (approvalId) => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "credential:turn-1",
        sessionId: "session-1",
        action: "request_credential",
        risk_class: "hitl",
        secret: true,
        label: "instagram password",
      },
      messages: [
        {
          id: "a1",
          role: "assistant",
          text: "",
          blocks: [
            {
              type: "ask_human",
              id: "ask1",
              approvalId,
              question: "Enter your instagram password",
              options: [],
              status: "pending",
            },
          ],
        },
      ],
    });

    const originalSubmit = useAppStore.getState().submitCredential;
    const submit = vi.fn(async () => { useAppStore.setState({ pendingApproval: null }); });
    useAppStore.setState({ submitCredential: submit });
    try {
      render(<><ApprovalBanner /><AskHumanBlock messageId="a1" block={{
        type: "ask_human", id: "ask1", approvalId,
        question: "Enter your instagram password", options: [], status: "pending",
      }} /></>);
      expect(screen.getAllByRole("button", { name: /send securely/i })).toHaveLength(1);
      fireEvent.change(screen.getByPlaceholderText(/instagram password/i), { target: { value: "test-secret" } });
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: /send securely/i })); });
      expect(submit).toHaveBeenCalledWith("test-secret");
      expect(document.querySelector('input[type="password"]')).toBeNull();
      expect(screen.queryByText("test-secret")).toBeNull();
    } finally {
      act(() => { useAppStore.setState({ submitCredential: originalSubmit }); });
    }
  });

  it("diverts a credential-intent ask_human to the masked secure prompt", () => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "question:turn-9",
        sessionId: "session-1",
        action: "ask_human",
        risk_class: "hitl",
        question: "What is the LinkedIn password for that account?",
      },
      messages: [],
    });

    render(<ApprovalBanner />);

    // Even though it came in as ask_human, the UI refuses the plain-text field
    // and renders the masked secure prompt instead.
    const pw = document.querySelector('input[type="password"]') as HTMLInputElement | null;
    expect(pw).not.toBeNull();
    expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
    expect(screen.getByRole("button", { name: /send securely/i })).toBeInTheDocument();
  });

  it("refuses a verification-code ask_human with no input field", () => {
    useAppStore.setState({
      pendingApproval: {
        approval_id: "question:turn-10",
        sessionId: "session-1",
        action: "ask_human",
        risk_class: "hitl",
        question: "Read me the 2FA code sent to your phone.",
      },
      messages: [],
    });

    render(<ApprovalBanner />);

    expect(screen.getByText(/Blocked for your safety/i)).toBeInTheDocument();
    expect(screen.getByText(/won't relay/i)).toBeInTheDocument();
    // No text or password input is offered at all.
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.getByRole("button", { name: /dismiss/i })).toBeInTheDocument();
  });
});

describe("chat-scoped permission decisions", () => {
  afterEach(() => { cleanup(); useAppStore.setState({pendingApproval:null}); });
  it("keeps a one-off decision from granting permissions to other chats", () => {
    useAppStore.setState({sessionId:"scoped-chat",pendingApproval:{approval_id:"scoped-one",action:"browser_click",detail:"Open sign-in",profile_scoped:true}});
    render(<ApprovalBanner/>);
    expect(screen.getByRole("button",{name:"Allow once"})).toBeInTheDocument();
    expect(screen.queryByRole("button",{name:"All routine permissions"})).not.toBeInTheDocument();
    expect(screen.queryByRole("button",{name:"Full access"})).not.toBeInTheDocument();
  });
});
