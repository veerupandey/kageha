import * as Popover from "@radix-ui/react-popover";
import type { BrowserState } from "../api/types";

const reasons: Record<string, string> = {
  off: "Decision actions are off for this run.",
  ready: "Enabled for supported browser steps in this chat.",
  compare_ready: "The decision model compares eligible steps while the regular agent executes.",
  eligible: "The current browser step is eligible for a typed decision.",
  routed: "The decision model proposed a browser action.",
  sent_to_executor: "The browser executor received the decision. Waiting for action verification.",
  verified: "The browser verified the action chosen by the decision model.",
  blocked: "The browser executor blocked the proposed action.",
  failed: "The browser executor reported an action error.",
  unconfirmed: "The browser has not verified the result of this action.",
  shadow: "Decision results are recorded for comparison. The regular agent controls actions.",
  caller_values_required: "This chat has no caller-provided field values. The regular agent handles the browser without an extra decision call.",
  fallback: "The decision result did not qualify for direct execution. The regular agent handles the next step.",
  decision_unavailable: "The decision service could not provide a usable result. The regular agent continues.",
  invalid_configuration: "The saved decision configuration is unavailable for this run. The regular agent continues; review Decision settings for the next run.",
  user_control: "You have browser control. Decision actions are paused.",
  context_changed: "The request or selected tab changed while the model was evaluating this step.",
  pending_action: "An earlier action needs verification before another action on this page.",
  no_supported_candidates: "The page has no eligible action for the current request. The regular agent handles this step.",
  unchanged_state: "This page state was already evaluated. The regular agent handles the next step.",
  main_executor_layout: "This step needs the regular agent to handle page layout.",
  budget_exhausted: "The decision budget for this run is used. The regular agent continues.",
};

const fallbackReasons: Record<string, string> = {
  low_score: "The proposed action did not meet the score cutoff.",
  not_applicable: "The model found no applicable action.",
  missing_confidence: "The provider did not return the selected confidence score.",
  credentials_unavailable: "Provider credentials are unavailable.",
  provider_authentication_failed: "The provider rejected the configured credentials.",
  timeout: "The provider reached the decision time budget.",
  rate_limited: "The provider rate-limited the request.",
  invalid_response: "The provider returned an invalid typed response.",
  provider_error: "The provider rejected the typed request.",
  provider_unavailable: "The decision provider is unavailable.",
  context_limit: "The page context exceeded the provider limit.",
  audit_unavailable: "The action could not be recorded for verification.",
};

export function BrowserDecisionRoute({ route }: { route: BrowserState["decisionRoute"] }) {
  if (!route || route.status === "off") return null;
  const label = route.status === "verified" ? "Decision verified"
    : ["routed", "sent_to_executor"].includes(route.status) ? "Decision route"
    : ["ready", "eligible"].includes(route.status) ? "Decisions enabled"
    : ["shadow", "compare_ready"].includes(route.status) ? "Compare mode"
    : route.status === "user_control" ? "Decisions paused"
    : route.status === "blocked" ? "Decision blocked"
    : route.status === "unconfirmed" ? "Decision unconfirmed" : "Regular agent";
  const compactLabel = route.status === "verified" ? "Verified"
    : ["routed", "sent_to_executor"].includes(route.status) ? "Decision"
    : ["ready", "eligible"].includes(route.status) ? "Enabled"
    : ["shadow", "compare_ready"].includes(route.status) ? "Compare"
    : route.status === "user_control" ? "Paused"
    : route.status === "blocked" ? "Blocked"
    : route.status === "unconfirmed" ? "Unconfirmed" : "Agent";
  const percentage = (value: number) => `${Math.round(value * 100)}%`;
  return <Popover.Root>
    <Popover.Trigger asChild>
      <button type="button" className="browser-control-button" aria-label="Browser execution route"><span className="hidden sm:inline">{label}</span><span className="sm:hidden">{compactLabel}</span></button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content className="browser-view-menu space-y-2 p-3 text-xs" style={{ width: "min(22.5rem, calc(100vw - 1.5rem))", minWidth: 0, maxWidth: "var(--radix-popover-content-available-width)" }} side="top" align="end" sideOffset={8} aria-label="Browser execution route details">
        <p className="font-medium text-ink">Latest browser route</p>
        <p className="leading-relaxed text-muted">{reasons[route.status] ?? "The regular agent handles this step."}</p>
        {route.reason && fallbackReasons[route.reason] && <p className="text-muted">{fallbackReasons[route.reason]}</p>}
        {Boolean(route.verified_actions) && <p>Verified decision actions this turn: {route.verified_actions}</p>}
        {typeof route.elapsed_ms === "number" && route.elapsed_ms > 0 && <p>Decision time: {route.elapsed_ms} ms</p>}
        {route.last_action && <p>Last action: {route.last_action} · {route.execution}</p>}
        {route.model && <p className="break-words text-muted">Decision model: {route.model}</p>}
        {typeof route.cutoff === "number" && <p className="text-muted">
          {route.score_metric === "provider_confidence" ? "Provider confidence" : "Selected probability"} cutoff: {percentage(route.cutoff)}
          {typeof route.batch_cutoff === "number" && ` · Batch: ${percentage(route.batch_cutoff)}`}
        </p>}
        {typeof route.minimum_score === "number" && <p className="text-muted">Decision score: {percentage(route.minimum_score)}</p>}
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
