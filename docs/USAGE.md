# Using Kageha

Start with the [quick start](../README.md#quick-start). In **Settings → Models**, connect a provider and select the model for your chat.

## Chat and files

- Attach source files from the composer. Ask Kageha to save its output in the workspace, then open or download it from **Files**.
- Use a follow-up in the same chat to revise an earlier result. During a run, you can queue the next request or steer the current turn.
- Use **Projects** to keep related chats, files, and workspaces together. Review remembered lessons before reusing them in another task.

## Planning and approvals

| Mode | Use it to |
| --- | --- |
| `/normal` | Ask a question or give Kageha a task in ordinary chat. |
| `/plan` | Review a plan and choose **Build** when you are ready. |
| `/goal` | Work toward a saved objective and review progress across turns. |

Use **Ask** permissions when you want to review sensitive actions before they run. Review the target and requested action in the approval card. Check the tool activity and result before retrying interrupted steps. Use the Jobs view to check persisted background work or cancel a running job.

## Choose where work runs

The chat can use the current host, a Docker workstation, or a configured SSH host. The execution picker shows the selected destination. Use **Browser** to watch or take over web activity and **Computer** for a live desktop when one is configured. See [browser setup](BROWSER.md), [Linux deployment](LINUX_DEPLOYMENT.md), and the [native macOS helper](../crates/kageha-computer/README.md).

## Extend your workspace

- Set up [agents and teams](AGENTS.md) to give specialists their own instructions, tools, and reference material.
- Connect GitHub, Gmail, Google Calendar, or other available services in **Settings → Integrations**. Use **Extensions** for [plugins, skills, and MCP](PLUGIN_CONNECTIONS.md).
- Configure [messaging channels](CHANNELS.md) to send requests and receive results away from the Web UI.
