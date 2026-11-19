import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { PendingAttachments } from "./PendingAttachments";

describe("PendingAttachments", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("keeps a pasted image thumbnail backed by a live object URL", async () => {
    const createObjectURL = vi.fn().mockReturnValue("blob:clipboard-preview");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { createObjectURL, revokeObjectURL });
    const file = new File([new Uint8Array([1, 2, 3])], "clipboard.png", {
      type: "image/png",
    });

    render(<PendingAttachments files={[file]} onRemove={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "clipboard.png" })).toHaveAttribute(
        "src",
        "blob:clipboard-preview",
      );
    });
    expect(createObjectURL).toHaveBeenCalledWith(file);
    expect(revokeObjectURL).not.toHaveBeenCalled();
  });
});
