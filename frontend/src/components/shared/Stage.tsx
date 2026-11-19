import type { ReactNode } from "react";
import { cn } from "../../lib/cn";

export function StageBoot({
  title,
  sub,
  failed,
}: {
  title: string;
  sub: string;
  failed?: boolean;
}) {
  return (
    <div className={cn("stage-boot", failed && "is-failed")} role="status">
      <div className="stage-boot-ring" />
      <p className="stage-boot-title">{title}</p>
      <p className="stage-boot-sub">{sub}</p>
    </div>
  );
}

export function StageWindow({
  children,
  className,
  overlay,
}: {
  children: ReactNode;
  className?: string;
  overlay?: boolean;
}) {
  return (
    <div className={cn("stage-window-well", overlay && "is-overlay", className)}>
      <div className="stage-window">{children}</div>
    </div>
  );
}

export function StageDriver({
  controlling,
  onAgent,
  onYou,
  label,
}: {
  controlling: boolean;
  onAgent: () => void;
  onYou: () => void;
  label: string;
}) {
  return (
    <div className="stage-switch" data-who={controlling ? "you" : "agent"} role="tablist" aria-label={label}>
      <span className="stage-switch-thumb" aria-hidden="true" />
      <button type="button" role="tab" data-who="agent" aria-selected={!controlling} onClick={onAgent}>
        <span className="stage-switch-dot" />
        Agent
      </button>
      <button type="button" role="tab" data-who="you" aria-selected={controlling} onClick={onYou}>
        You
      </button>
    </div>
  );
}

export function StageIdentity({
  live,
  children,
}: {
  live?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="stage-url">
      <span
        className={cn("stage-identity-pulse", live && "is-live")}
        aria-hidden="true"
      />
      {children}
    </div>
  );
}
