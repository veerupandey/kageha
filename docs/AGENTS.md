# Agents and teams

An agent is a named specialist with its own instructions, selected model and skills, tool access, and knowledge. A team groups specialists for work that benefits from delegation.

## Set up a specialist

1. In the Web UI sidebar, open **Agents** and choose **New agent**.
2. Give the agent a name and instructions. Set its model and tool access, and pin skills it should use.
3. In the agent's knowledge section, upload the reference material it needs. Assign shared sources only when the specialist should use them.
4. Open the agent from the sidebar or choose it in the chat composer. You can start a message with an agent mention, such as `@researcher Compare these three approaches`.

The chat records which specialist produced each reply. Use the composer selector to address Kageha again. Use the agent's history when you need to review or restore an earlier configuration.

## Create a team

Create a team in the Web UI, add its members, and choose a dispatch mode. Coordinate lets the lead agent assign work. Parallel runs independent work, while Sequence passes work through the roster in order. Start a team conversation from the sidebar or use a team mention such as `&research-writers` in a new conversation.

Review worker progress and returned artifacts from the parent chat. A running worker uses its own scoped workspace and tool permissions. Use the parent chat's follow-up controls to steer the work.

The [SDK guide](../sdk/AGENT.md) describes how to work with agents and turns programmatically.
