/**
 * Compact detail lines for the agent-loop events of design section 16. The
 * Host gives each one a `label`; this adds at most one short line from its
 * payload so the activity list stays readable.
 */
const text = (value: unknown) => (typeof value === "string" ? value.trim() : typeof value === "number" ? String(value) : "");

const join = (...parts: unknown[]) => parts.map(text).filter(Boolean).join(" · ");

export const LOOP_EVENT_KINDS = new Set([
  "completion_check",
  "outcome_rejected",
  "outcome_amended",
  "outcome_blocked",
  "escalation",
  "recovery",
  "continuation",
  "component_restarted",
  "component_degraded",
  "model_fallback",
  "skill_learned",
  "skill_degraded",
  "disk_low",
  "waiting_for_unlock",
]);

/** The payload of a loop event, which may itself carry the runtime frame. */
function detailSource(payload: Record<string, unknown>): Record<string, unknown> {
  const nested = payload.payload;
  return nested && typeof nested === "object" && !Array.isArray(nested)
    ? { ...(nested as Record<string, unknown>), ...payload }
    : payload;
}

export function loopEventDetail(kind: string, payload: Record<string, unknown> | undefined): string[] {
  if (!LOOP_EVENT_KINDS.has(kind) || !payload) return [];
  const p = detailSource(payload);
  let line = "";
  switch (kind) {
    case "completion_check": {
      const unmet = Array.isArray(p.unmet) ? p.unmet.map(text).filter(Boolean) : [];
      line = join(
        p.status,
        p.stop_reason ? `stop: ${text(p.stop_reason)}` : "",
        unmet.length ? `unmet: ${unmet.slice(0, 3).join(", ")}${unmet.length > 3 ? "…" : ""}` : "",
      );
      break;
    }
    case "escalation":
      line = join(p.from && p.to ? `${text(p.from)} → ${text(p.to)}` : p.to, p.stale_steps ? `${text(p.stale_steps)} steps without progress` : "");
      break;
    case "model_fallback":
      line = join(p.role, p.from && p.to ? `${text(p.from)} → ${text(p.to)}` : p.to, p.cause);
      break;
    case "recovery":
      line = join(p.cause, p.action, typeof p.result === "string" ? p.result : "");
      break;
    case "component_restarted":
    case "component_degraded":
      line = join(p.component || p.name, p.reason || p.cause);
      break;
    case "outcome_rejected":
    case "outcome_amended":
    case "outcome_blocked":
      line = join(p.outcome_id, p.reason || p.category);
      break;
    case "skill_learned":
    case "skill_degraded":
      line = join(p.name || p.skill, p.reason);
      break;
    default:
      line = join(p.reason || p.message);
  }
  return line ? [line.length > 160 ? `${line.slice(0, 159)}…` : line] : [];
}
