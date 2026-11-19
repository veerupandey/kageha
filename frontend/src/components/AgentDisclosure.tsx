import type { ReactNode } from "react";
import { agentKey, useAgentValue } from "../lib/agentContinuity";

export function AgentDisclosure({ scope, name, children, className }: { scope: string; name: string; children: ReactNode; className?: string }) {
  const [saved, setOpen] = useAgentValue(agentKey(scope, `disclosure:${name}`), false);
  return <details className={className} open={saved === true} onToggle={event => {
    const open = event.currentTarget.open;
    if (open !== (saved === true)) setOpen(open);
  }}>{children}</details>;
}
