import { useEffect, useRef, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as Dialog from "@radix-ui/react-dialog";
import { api } from "../api/client";
import { Icon } from "../lib/icons";
import { cn } from "../lib/cn";

type Profile = "ask" | "auto_review" | "full";
interface Permissions {
  profile: Profile;
  requirements?: {
    allowed_profiles: Profile[];
    forced_profile?: Profile | null;
    allow_review_override?: boolean;
  };
  denials?: {
    record: {
      id: string;
      revision: number;
      request: {
        authority: Record<string, unknown>;
        detail: string;
        action: string;
      };
    };
    review?: { rationale: string };
  }[];
  legacy_routine?: boolean;
  scope?: "ask" | "session" | "full";
  owner_session?: string | null;
  reviewer_available: boolean;
  chat_scoped_profiles: boolean;
}
const choices = [
  {
    id: "ask",
    label: "Ask for approval",
    detail: "Ask before actions that require approval.",
  },
  {
    id: "auto_review",
    label: "Approve for me",
    detail:
      "A reviewer checks eligible actions and asks you when unsure.",
  },
  {
    id: "full",
    label: "Full access",
    detail:
      "Skip individual approval prompts within this chat’s tool limits.",
  },
] as const;
export function PermissionMenu({
  sessionId,
  ensureSession,
}: {
  sessionId: string | null;
  ensureSession?: (beforeOpen: (sessionId: string) => Promise<void>) => Promise<{ sessionId: string }>;
}) {
  const loadedSession = useRef(sessionId);
  const currentSession = useRef(sessionId);
  currentSession.current = sessionId;
  const [data, setData] = useState<Permissions>();
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const update = () => setRevision((r) => r + 1);
    window.addEventListener("kageha-permissions-changed", update);
    return () =>
      window.removeEventListener("kageha-permissions-changed", update);
  }, []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [reviewOpen, setReviewOpen] = useState(false);
  const [retryMessage, setRetryMessage] = useState("");
  useEffect(() => {
    let active = true;
    if (loadedSession.current !== sessionId) {
      setData(undefined);
      setReviewOpen(false);
      setRetryMessage("");
      loadedSession.current = sessionId;
    }
    setError("");
    setConfirm(false);
    if (sessionId || ensureSession)
      void api<Permissions>(
        sessionId ? `/api/permissions?session_id=${encodeURIComponent(sessionId)}` : "/api/permissions",
      )
        .then((value) => {
          if (active) {
            if (!value.chat_scoped_profiles)
              setError("Chat permissions require the current Rust backend.");
            else setData(value);
          }
        })
        .catch(() => {
          if (active)
            setError(
              "Could not load permissions. Try again after reconnecting.",
            );
        });
    return () => {
      active = false;
    };
  }, [sessionId, revision, ensureSession]);
  const save = async (profile: Profile, confirmFull = false) => {
    if (busy || (!sessionId && (!ensureSession || (profile === "ask" && !data?.legacy_routine)))) return;
    const updateProfile = (id: string) =>
      api<Permissions>("/api/permissions", {
        method: "POST",
        body: JSON.stringify({ session_id: id, profile, confirm_full: confirmFull }),
      });
    setBusy(true);
    setError("");
    try {
      if (!sessionId && ensureSession) {
        await ensureSession(async (id) => { await updateProfile(id); });
        setConfirm(false);
        return;
      }
      if (!sessionId) return;
      const value = await updateProfile(sessionId);
      if (currentSession.current === sessionId) {
        setData(value);
        setConfirm(false);
      }
    } catch (e) {
      if (currentSession.current === sessionId)
        setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const selected = choices.find((c) => c.id === (data?.requirements?.forced_profile ?? data?.profile));
  const usesWorkspaceDefault = Boolean(data?.legacy_routine && !data.requirements?.forced_profile);
  const modeLabel = usesWorkspaceDefault ? "Workspace default" : selected?.label;
  return (
    <>
      <DropdownMenu.Root>
        <DropdownMenu.Trigger asChild>
          <button
            type="button"
            id="btn-ask-mode"
            data-ask={data?.profile ?? "unknown"}
            disabled={busy}
            aria-label={`Permissions: ${modeLabel ?? "not loaded"}`}
            className={cn(
              "inline-flex h-7 items-center gap-1.5 rounded-lg px-2 text-xs font-medium outline-none hover:bg-line/70 focus-visible:ring-2 focus-visible:ring-accent",
              selected?.id === "full"
                ? "bg-warn-soft text-warn"
                : "text-muted",
            )}
          >
            {selected?.id === "full" ? <Icon.Alert size={14} /> : <Icon.Shield size={14} />}
            {busy
              ? "Saving…"
              : (modeLabel ?? "Permissions")}
            <Icon.Chevron size={11} className="opacity-65" />
          </button>
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content
            side="top"
            sideOffset={8}
            collisionPadding={12}
            style={{
              maxHeight:
                "min(calc(100dvh - 24px), var(--radix-dropdown-menu-content-available-height))",
            }}
            align="start"
            className="z-50 w-[min(23rem,calc(100vw-1.5rem))] overflow-y-auto rounded-xl border border-line bg-surface p-1.5 text-ink shadow-2xl"
          >
            <DropdownMenu.Label className="flex items-center justify-between gap-3 px-2.5 py-2 text-xs font-medium text-muted">
              Action approvals
              <span className="rounded-md bg-line/60 px-1.5 py-0.5 text-[11px]">{sessionId ? "This chat" : "New chat"}</span>
            </DropdownMenu.Label>
            {error && (
              <DropdownMenu.Item
                className="rounded-lg px-3 py-2 text-sm text-accent outline-none data-[highlighted]:bg-line/60"
                onSelect={() => setRevision((r) => r + 1)}
              >
                Retry loading permissions
              </DropdownMenu.Item>
            )}
            {data?.requirements?.forced_profile && (
              <p className="px-3 py-2 text-xs text-muted">
                Your administrator controls the permission mode.
              </p>
            )}
            {data?.owner_session && data.owner_session !== sessionId && (
              <p className="px-3 py-2 text-xs text-muted">
                Change permissions in the parent chat.
              </p>
            )}
            {usesWorkspaceDefault && (
              <p className="px-2.5 pb-2 text-xs leading-snug text-muted">
                {data?.scope === "full"
                  ? "This chat inherits the workspace’s full access setting."
                  : "This chat inherits the workspace’s routine approvals."}
                {" Choose a mode below to set it for this chat."}
              </p>
            )}
            <DropdownMenu.RadioGroup
              value={usesWorkspaceDefault ? "" : (selected?.id ?? "")}
              onValueChange={(value) => {
                if (value === "full") setConfirm(true);
                else void save(value as Profile);
              }}
            >
              {choices.map((choice) => (
                <DropdownMenu.RadioItem
                  key={choice.id}
                  value={choice.id}
                  disabled={
                    !data ||
                    (!sessionId && !ensureSession) ||
                    (!!data.owner_session &&
                      data.owner_session !== sessionId) ||
                    (data.requirements?.allowed_profiles &&
                      !data.requirements.allowed_profiles.includes(
                        choice.id,
                      )) ||
                    (!!data.requirements?.forced_profile &&
                      data.requirements.forced_profile !== choice.id) ||
                    busy ||
                    (choice.id === "auto_review" && !data.reviewer_available)
                  }
                  className={cn(
                    "group grid cursor-pointer grid-cols-[18px_1fr_16px] items-start gap-2.5 rounded-lg px-2.5 py-2 outline-none data-[state=checked]:bg-line/35 data-[highlighted]:bg-line/70 data-[disabled]:cursor-default data-[disabled]:opacity-60",
                    choice.id === "full" && "text-warn",
                  )}
                >
                  {choice.id === "full" ? <Icon.Alert size={17} className="mt-0.5" /> : choice.id === "auto_review" ? <Icon.Eye size={17} className="mt-0.5 text-muted" /> : <Icon.Shield size={17} className="mt-0.5 text-muted" />}
                  <span className="min-w-0 flex-1">
                    <span className="block text-sm font-medium">
                      {choice.label}
                    </span>
                    <span className="mt-0.5 block text-xs leading-snug text-muted">
                      {choice.id === "auto_review" &&
                      data &&
                      !data.reviewer_available
                        ? "Configure a model to enable automatic review."
                        : choice.detail}
                    </span>
                  </span>
                  <DropdownMenu.ItemIndicator className="mt-0.5">
                    <Icon.Check size={15} />
                  </DropdownMenu.ItemIndicator>
                </DropdownMenu.RadioItem>
              ))}
            </DropdownMenu.RadioGroup>
            {!!sessionId && !!data?.denials?.length && (
              <DropdownMenu.Item
                className="rounded-xl px-3 py-2 text-sm text-accent outline-none data-[highlighted]:bg-line/60"
                onSelect={() => {
                  setRetryMessage("");
                  setReviewOpen(true);
                  setRevision((r) => r + 1);
                }}
              >
                Review denied actions
              </DropdownMenu.Item>
            )}
            <p className="mt-1 border-t border-line px-2.5 py-2 text-[11px] leading-snug text-muted">
              {data?.legacy_routine && "A legacy routine grant is active. "}
              Tool and workspace limits still apply.
            </p>
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
      {error && (
        <span role="alert" className="max-w-xs text-xs text-warn">
          {error}
        </span>
      )}
      <Dialog.Root open={reviewOpen} onOpenChange={setReviewOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-50 bg-black/60" />
          <Dialog.Content className="fixed top-1/2 left-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(32rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-2xl border border-line bg-surface p-6 text-ink shadow-xl">
            <Dialog.Title className="text-lg font-semibold">
              Review denied actions
            </Dialog.Title>
            <Dialog.Description className="mt-2 text-sm text-muted">
              A retry applies only to the exact action and is reviewed again. It
              does not send browser input. After requesting a retry, ask Kageha
              to try that action again.
            </Dialog.Description>
            {error && (
              <p role="alert" className="mt-3 text-sm text-warn">
                {error}
              </p>
            )}
            <div className="mt-4 space-y-3">
              {data?.denials?.map(({ record, review }) => (
                <div
                  key={record.id}
                  className="rounded-xl border border-line p-3"
                >
                  <p className="text-sm">{record.request.detail}</p>
                  <p className="mt-2 text-xs text-muted">{review?.rationale}</p>
                  <button
                    type="button"
                    disabled={
                      busy || data.requirements?.allow_review_override === false
                    }
                    className="mt-3 rounded-lg border border-line px-3 py-2 text-xs disabled:opacity-40"
                    onClick={async () => {
                      setBusy(true);
                      try {
                        await api("/api/review-overrides", {
                          method: "POST",
                          body: JSON.stringify({
                            approval_id: record.id,
                            authority: record.request.authority,
                            expected_revision: record.revision,
                          }),
                        });
                        setRetryMessage(
                          "One reviewed retry requested. No action was executed.",
                        );
                      } catch (e) {
                        setRetryMessage(
                          e instanceof Error ? e.message : String(e),
                        );
                      } finally {
                        setBusy(false);
                      }
                    }}
                  >
                    Request one reviewed retry
                  </button>
                </div>
              ))}
            </div>
            {retryMessage && (
              <p role="status" className="mt-3 text-sm text-muted">
                {retryMessage}
              </p>
            )}
            <Dialog.Close asChild>
              <button
                type="button"
                className="mt-4 rounded-lg border border-line px-3 py-2 text-sm"
              >
                Close
              </button>
            </Dialog.Close>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <Dialog.Root open={confirm} onOpenChange={setConfirm}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-50 bg-black/60" />
          <Dialog.Content className="fixed top-1/2 left-1/2 z-50 max-h-[calc(100dvh-2rem)] overflow-y-auto w-[min(26rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-2xl border border-line bg-surface p-6 shadow-xl">
            <Dialog.Title className="text-lg font-semibold text-ink">
              Enable Full access?
            </Dialog.Title>
            <Dialog.Description className="mt-3 text-sm leading-relaxed text-muted">
              Kageha can change files, use connected tools and submit browser
              actions without individual prompts in {sessionId ? "this chat" : "the new chat"}. This can expose
              data or make irreversible changes. Browser access, destination
              checks and tool limits remain in place.
            </Dialog.Description>
            {error && <p role="alert" className="mt-3 text-sm text-warn">{error}</p>}
            <div className="mt-6 flex justify-end gap-2">
              <Dialog.Close asChild>
                <button
                  type="button"
                  className="rounded-lg border border-line px-3 py-2 text-sm"
                >
                  Cancel
                </button>
              </Dialog.Close>
              <button
                type="button"
                disabled={busy}
                onClick={() => void save("full", true)}
                className="rounded-lg bg-warn px-3 py-2 text-sm font-semibold text-black disabled:opacity-50"
              >
                {busy ? "Saving…" : "Enable Full access"}
              </button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
