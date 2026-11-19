import { api } from "./client";

export type IntegrationAgent = { id: string; name: string };

let pending: Promise<{ agents: IntegrationAgent[] }> | undefined;

/** Share the concurrent directory lookup. A later visit fetches fresh assignments. */
export function integrationAgents() {
  pending ??= api<{ agents: IntegrationAgent[] }>("/api/agents")
    .finally(() => { pending = undefined; });
  return pending;
}
