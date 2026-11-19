# Kageha TypeScript/JavaScript client

Local prerelease SDK, independent of WebUI. Install this directory as a local npm
dependency. Node 22+; no runtime npm dependencies.

```js
import { AgentClient } from "@kageha/client/agent";
const client = new AgentClient(endpoint, operatorToken);
const run = await client.run(portableRunSpec);
for await (const event of client.events(run.spec.id)) {
  console.log(event.sequence, event.record.state);
}
```

Also provides `status`, `steer`, `cancel`, `approvals`, `approval` and `decide`.
Types ship with the package. Decisions require the exact reviewed authority and
revision. Events are resumable lifecycle records, not model token deltas. No
mutation is automatically retried. Closing a stream does not cancel the server run.

The repository's `sdk/AGENT.md` contains complete requests, recovery guidance and
examples for Rust and Python. The root export additionally provides `MemoryClient`.
