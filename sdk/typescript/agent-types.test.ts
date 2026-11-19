import {
  AgentClient,
  type RunSpec,
  type ApprovalDecision,
  type RunEvent,
} from "@kageha/client";
export async function typedUsage(client: AgentClient, spec: RunSpec) {
  const run = await client.run(spec);
  await client.status(run.spec.id);
  await client.steer(run.spec.id, {
    id: "direction-1",
    body: "Use the revised objective",
  });
  for await (const event of client.events(run.spec.id, { after: 0 })) {
    const typed: RunEvent = event;
    if (typed.record.state === "succeeded") break;
  }
  const page = await client.approvals({ session: spec.session, pending: true });
  for (const pending of page.approvals) {
    const record = await client.approval(pending.id);
    const decision: ApprovalDecision = {
      authority: record.request.authority,
      expected_revision: record.revision,
      approved: false,
      scope: "once",
    };
    await client.decide(record.id, decision);
    // @ts-expect-error An exact authority and revision are required.
    await client.decide(record.id, { approved: true });
  }
  await client.cancel(run.spec.id);
}
