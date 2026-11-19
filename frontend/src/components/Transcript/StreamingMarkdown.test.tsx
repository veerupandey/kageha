import { act, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { StreamingMarkdown } from "./StreamingMarkdown";

describe("StreamingMarkdown", () => {
  it("renders formatted markdown while streaming", async () => {
    const { container } = render(
      <StreamingMarkdown
        id="m1"
        text={"Hello **world**"}
        streaming
        sessionId="sess1"
      />,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 140));
    });
    expect(container.querySelector(".ka-stream-cursor")).toBeTruthy();
    expect(container.querySelector("strong")?.textContent).toBe("world");
  });

  it("renders markdown after streaming completes", () => {
    const { container } = render(
      <StreamingMarkdown
        id="m2"
        text="Hello **world**"
        streaming={false}
        sessionId="sess1"
      />,
    );
    expect(container.querySelector("strong")?.textContent).toBe("world");
    expect(container.querySelector(".ka-stream-cursor")).toBeNull();
  });

  it("formats an unfinished code fence while streaming", async () => {
    const { container } = render(
      <StreamingMarkdown
        id="m3"
        text={'```python\nprint("hello")'}
        streaming
        sessionId="sess1"
      />,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 140));
    });
    expect(container.querySelector(".code-block-enhanced")).toBeTruthy();
    expect(container.querySelector(".code-block-lang")?.textContent).toBe("Python");
  });
});


it("publishes intermediate text while tokens arrive faster than the render interval", () => {
  vi.useFakeTimers();
  const { container, rerender, unmount } = render(<StreamingMarkdown id="continuous" text="Start" streaming sessionId="s1" />);
  try {
    for (let i = 1; i <= 8; i++) {
      rerender(<StreamingMarkdown id="continuous" text={`Token ${i}`} streaming sessionId="s1" />);
      act(() => { vi.advanceTimersByTime(40); });
      if (i === 3) expect(container.textContent).toContain("Token 3");
      if (i === 6) expect(container.textContent).toContain("Token 6");
    }
  } finally {
    unmount();
    vi.useRealTimers();
  }
});
