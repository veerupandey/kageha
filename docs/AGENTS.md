# Custom Agents and Teams

Kageha lets you build a roster of specialist agents, each with its own
instructions, skills, model preferences, and private knowledge base.  You
can address any agent by name from the composer, the CLI, messaging channels,
or by pinning it to a project directory.

---

## What is an agent?

An agent is a saved persona stored at `~/.kageha/agents/<id>.yaml`.  It
carries:

| Field | What it controls |
|-------|-----------------|
| `name` + `avatar` | Display name and emoji shown in the sidebar and thread list |
| `system_prompt` | Extra instructions prepended to every LLM context window for this agent |
| `pinned_skills` | Skills that always auto-load for this agent regardless of task text |
| `skills_restricted_to_pinned` | When `true`, disables global skill autoload — agent only ever uses its pinned skills |
| `tool_policy` | Per-agent allow / deny lists and risk-class overrides |
| `model_preferences` | Per-role model overrides: `planner`, `executor`, `fast_worker`, `embedding` |
| `knowledge` | Private zvec vector store + optional MCP or HTTP retriever |

The default persona is snapshotted when a thread is created. An explicitly
addressed specialist is snapshotted on its first invocation in that chat. Editing the
agent later never changes existing threads.

---

## Creating an agent

### Web UI

Open the sidebar → **Agents** section → click **+** (New agent).

Fill in:

1. **Name** — what shows in the sidebar (e.g. `Researcher`)
2. **Avatar** — optional emoji (e.g. `🔬`)
3. **ID** — auto-generated from the name; used as `@researcher` in messages
4. **Instructions** — the agent's system prompt:
   ```
   You are a deep research specialist. Always cite your sources. Prefer
   primary references over aggregator sites.
   ```
5. **Pinned skills** — check any skills that should always load (e.g. `web_browse`)
6. **Knowledge** — optionally upload PDFs, markdown, or other documents to
   give the agent private reference material
7. **Access & model preferences** (advanced) — deny specific tools, pin a
   faster or stronger model

Click **Create agent**.

### CLI

```bash
# Minimal
kageha agent create researcher \
  --name "Researcher" \
  --avatar "🔬" \
  --prompt "You are a deep research specialist. Always cite sources."

# With pinned skills and a restricted skill set
kageha agent create job-hunter \
  --name "Job Hunter" \
  --avatar "💼" \
  --prompt "You find and apply to senior AI engineering jobs for Rakesh." \
  --skill job-apply \
  --restrict-to-pinned

# From a file (for long prompts)
kageha agent create writer \
  --name "Writer" \
  --prompt-file ./prompts/writer.md
```

### Edit / delete

```bash
kageha agent list                       # see all agents
kageha agent show researcher            # full config
kageha agent edit researcher --name "Senior Researcher"
kageha agent delete researcher
```

---

## Using an agent

### Calling an agent by @mention (anywhere)

Type `@` followed by the agent id at the **start** of any message.  Works in
the Web UI composer, `kageha chat`, messaging channels (Telegram, WhatsApp,
iMessage), and `kageha run`:

```
@researcher find the top 5 AI companies hiring in Vancouver

@writer draft a cover letter for the Elastic Principal AI Engineer role

@job-hunter apply to the Workday Vancouver posting
```

The `@agentname` prefix is stripped from the message before it reaches the
agent — the agent receives only the clean task text.

Rules:
- The agent must already exist (`kageha agent list` to check).
- In the Web UI and app-server chat, an explicit unknown agent returns an error.
- In an existing Web UI chat, `@agent` or the recipient selector addresses that
  specialist in the same conversation. Its model history contains its own
  exchanges. The chat's original default persona remains unchanged.
- Replies persist the speaker's name and avatar, independently of the current
  composer selection. Select Kageha to return to the main agent.
- Other transports retain their existing session-routing behavior.

### Clicking in the sidebar

Click any agent name in the left sidebar under **Agents** → opens a new
thread bound to that agent.

### Quick-pick chips above the composer

All your agents appear as clickable pills above the textarea.  Click one to
activate it; click it again (or click **×** in the toolbar chip) to return to
Default.

### CLI flags

```bash
kageha run "@researcher find AI jobs in Vancouver"   # @mention syntax

kageha run "find AI jobs" --agent researcher         # explicit flag

kageha chat --agent researcher                       # whole session
```

`--agent` validates the id at startup and exits with a clear error if the
agent is not found.

---

## Project-default agent

Pin a default agent for a directory so you never have to type `--agent` or
`@mention` again:

```bash
# Pin for the current directory
kageha agent set-default job-hunter

# Pin for a specific project
kageha agent set-default researcher --project ~/work/ai-research

# Clear it
kageha agent set-default none
```

This writes a one-line `.kageha/agent` file in the project root.  Every
`kageha run` and `kageha chat` in that directory automatically uses the
pinned agent for new sessions.

The Web UI reads the same file when `project_root` is set on a turn request —
so the project-default agent is active there too.

**Priority order** (highest wins):
1. `--agent` CLI flag or explicit `agent_id` from the Web UI picker
2. `@agentname` or `&teamname` mention in the message
3. `.kageha/agent` project-default file
4. Default agent (`main`)

---

## Agent knowledge

Each agent has a private vector store.  Upload documents in the Web UI
(agent editor → Knowledge section) or via the CLI:

```bash
kageha agent knowledge researcher --add ~/docs/ai-landscape-2026.pdf
```

By default, the agent also inherits your workspace knowledge bases.  To make
it search only its own documents:

```bash
kageha agent knowledge researcher --only
```

Custom retrievers (MCP tool or HTTP endpoint) can be set via the agent editor
for external knowledge systems.

---

## Teams

A team is a named group of agents.  Addressing a team routes the task to the
coordinator agent, which can delegate to its teammates via `spawn_subagent`.

### Dispatch modes

| Mode | Behaviour |
|------|-----------|
| `coordinate` | First agent is the coordinator. Receives the task + team roster context, delegates via `spawn_subagent` / `spawn_subagents`. Best for "chief assigns work" workflows. |
| `parallel` | All agents receive the same task simultaneously. Coordinator synthesises their combined output. |
| `sequence` | Agents run one after another. Each receives the original task plus the previous agent's output as context. Good for research → draft → review pipelines. |

### Creating a team

```bash
# Two-agent research + writing team
kageha team create research-writers \
  --name "Research Writers" \
  --agent researcher \
  --agent writer \
  --mode coordinate

# Three-agent pipeline
kageha team create review-pipeline \
  --name "Review Pipeline" \
  --agent researcher \
  --agent writer \
  --agent editor \
  --mode sequence \
  --description "Research, draft, then edit in sequence"
```

### Managing teams

```bash
kageha team list                   # all teams with mode and roster
kageha team show research-writers  # full details + usage hint
kageha team edit research-writers --agent researcher --agent writer --agent editor
kageha team delete research-writers
```

### Using a team

Type `&` followed by the team id at the start of a message:

```
&research-writers write a report on agentic AI trends in 2026

&review-pipeline draft a technical blog post about the Kageha agent system
```

Teams are also visible in the sidebar under **Teams** (only shown when at
least one team exists).  Clicking a team row activates the coordinator agent
and opens a new thread.

Team mentions retain their existing new-session-only routing rule.

---

## Agent identity in the UI

- **Sidebar → Agents** — lists all agents with avatar, name, and description.
  Active agent is highlighted.  Gear icon opens the editor.
- **Sidebar → Teams** — lists teams with mode icon (⌘ coordinate, ⇉ parallel,
  → sequence) and the full agent roster.
- **Composer** — use `@` autocomplete or the recipient selector to choose who
  replies. A compact selection chip identifies the current recipient.
- **Toolbar chip** — when an agent is active, its name (and avatar if set)
  appears in the composer toolbar.  Click **×** to return to Default.
- **Thread list** — each thread shows the agent's emoji or a coloured initial
  badge so you can see which agent handled it at a glance.  Default threads
  show nothing.

---

## Delegating work within a chat

Kageha can create temporary specialists or delegate to saved agents using
`spawn_subagent(task="Review the design", name="Architect")` or
`spawn_subagent(task="Check evidence", persona_id="researcher")`. Typed
`spawn_subagents` assignments also accept `name` and `persona_id`.

Each worker has a separate journal and model context. It remains inside its
parent chat's Agents activity and does not create a normal sidebar chat.
Inspect an agent to see its assignment, results, files, or send guidance.

Spawning alone starts no browser and creates no Git worktree. Workers share
project access by default; request `isolation="worktree"` for independent
edits. File mutations and shell commands take project leases at tool
boundaries. Browser and desktop operations have shared resource leases.
Private Chromium, Docker, and machine/CDP workers use separately owned tabs in the parent's browser;
resource ownership uses the parent's canonical directory, while artifacts
belong to the worker. A child does not close the parent's browser. Private browser transports are
published locally so separate worker processes attach to the same browser.
The embedded single-page guest cannot provide independent worker tabs;
select a Chromium/CDP browser for delegated browsing.

Direct `@agent` replies keep one visible conversation while each saved agent
has its own model history, plan, checkpoints, activated tools, and execution
state beneath that chat. Switching back restores that agent's context after
a restart. Only explicitly supplied files enter the selected context; output
links are projected into the shared chat. Replies retain the agent's name and
avatar. Runtime journal restores and pending questions resolve against the
selected context as well as its files.

Browser/desktop ownership leases cross worker process boundaries. Shared
computer observations become stale after another actor changes the desktop,
so an agent must observe again before acting on old coordinates.

Delegated assignments can carry a model-authored `contract` with
`expected_output`, `acceptance_criteria`, `constraints`, and a `verification`
choice. Verification defaults to required when a contract is supplied;
`not_required` needs an explicit reason. The runtime validates the contract
and preserves it in the job specification; it does not classify task intent
from words or tool names.

Agents can record concise, evidence-backed visible updates using
`report_agent_progress`. Progress never establishes completion or verification.
Before finishing, a specialist can use `submit_agent_contribution` to record
a finding, supporting references, limitations, and decisions to consider.
Optional `sources` records carry a `title`, exactly one observed `url` or
`artifact_path`, and an optional `locator` (page, table, or section). The UI
opens HTTP(S) sources and files matched to that worker's published artifact
manifest. Unpublished files and invalid targets remain labelled without links;
plain evidence notes never become guessed URLs. Source links do not certify
that a claim was independently verified.

These model-authored fields appear in contribution cards and the details panel;
failed or cancelled assignments retain them as partial findings. They do not
establish independently verified execution.

The parent receives fenced specialist data and reconciles it into one answer.
Continuation turns include required semantic checks for preserving recommendation
scope, distinguishing recorded validation from claims, and attributing limitations
to their actual sources. Unavailable or unresolved checks cannot certify a handoff.
Runtime events independently record tool actions, model waits, retries, and
verification. Worker lease health is distinct from action and model-response
timestamps. The UI labels a finished assignment as verified only when the
result contains recorded validation.

The browser preserves unsent specialist messages and question answers locally,
scoped to the chat and specialist (and question for answers). Retry IDs survive
reloads; restored drafts never send automatically. Accepted messages clear only
the matching draft, so a delayed response cannot erase newer text. Drafts for
finished specialists remain available to copy or discard. The selected specialist,
agent list expansion, and details disclosures restore from local view preferences;
restoration reloads live job data rather than caching execution results. Restricted
browser storage falls back to editing within the current mount.

Guidance has durable delivery and acknowledgement timestamps. The worker
uses `acknowledge_agent_guidance` to explain how it incorporated delivered
messages. Acknowledgement is the worker's statement, not proof of execution.
Background replies are queued transactionally with job completion and
projected idempotently into chat; interruption before delivery acknowledgement
leaves a pending publication for the worker service to retry.

Named assignments display their readable names in transcript cards as well as
in the agent list. Dependency IDs stay internal. An unnamed durable assignment
uses `Worker 1`, `Worker 2`, and so on; it does not require a saved persona.
Expanding a durable spawn card loads concise recorded status, the latest visible
update and next step, and the contribution when available. This is a summary of
observable work, not private reasoning. The default worker keeps its own context,
inherits parent restrictions, and starts optional resources only when needed.

Saved agents use captured instructions, pinned skills, model preferences,
and tool policies. Children inherit their parent's tool restrictions and
approval posture. Declaring a capability does not start that capability.

---

## Reference

### CLI commands

```
kageha agent list                      List all agents
kageha agent create <id> …             Create a new agent
kageha agent edit <id> …               Edit an existing agent
kageha agent delete <id>               Delete an agent
kageha agent show <id>                 Full config + effective settings
kageha agent knowledge <id>            Show or update private knowledge library
kageha agent set-default <id>          Pin as default for the current project
kageha agent set-default none          Clear the project default

kageha team list                       List all teams
kageha team create <id> …              Create a new team
kageha team show <id>                  Full details + usage hint
kageha team edit <id> …                Edit an existing team
kageha team delete <id>                Delete a team
```

### REST API

```
GET    /api/agents                     List all agents
POST   /api/agents                     Create
GET    /api/agents/<id>                Get + effective config
PUT    /api/agents/<id>                Update
DELETE /api/agents/<id>                Delete
GET    /api/agents/<id>/history        Version history
POST   /api/agents/<id>/rollback       Restore a snapshot
GET    /api/agents/<id>/knowledge      Knowledge base info
POST   /api/agents/<id>/knowledge/upload   Add a document

GET    /api/teams                      List all teams
POST   /api/teams                      Create
GET    /api/teams/<id>                 Get
PUT    /api/teams/<id>                 Update
DELETE /api/teams/<id>                 Delete
```

### Storage locations

| What | Where |
|------|-------|
| Agent configs | `~/.kageha/agents/<id>.yaml` |
| Agent history | `~/.kageha/agents/.history/<id>/` |
| Team configs | `~/.kageha/teams/<id>.yaml` |
| Agent knowledge | `~/.kageha/kb/agent-<id>/` |
| Project default | `<project>/.kageha/agent` |
