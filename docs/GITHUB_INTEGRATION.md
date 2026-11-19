# GitHub integration

Open **Settings → Integrations → GitHub**. This release supports one GitHub
account per Kageha home and read-only repository tools. It does not create commits,
issues, comments, or pull requests.

## Connect with a fine-grained token

1. On GitHub, create a fine-grained personal access token. Select the resource
   owner and repositories you want to expose. Grant **Contents**, **Issues**, and
   **Pull requests** read access (metadata access is included by GitHub).
2. Paste the token into the GitHub card and select **Connect token**.
3. Select repositories and agents, then **Save access**. No agents have access
   until explicitly assigned. Each selected agent can read every selected repository.
4. Ask an assigned agent: “List my GitHub repositories”, then “Read the README
   in owner/repository” or “Summarize pull request 12 and its changed files”.

Organization approval may be needed for the token. Use **Check connection** to
validate authentication and reload repositories. Token permissions still govern
individual operations; a successful account check does not guarantee every
repository permission. Use **Reconnect** to replace an expired token.

## Browser authorization with a GitHub App

Register a GitHub App with read access to Contents, Issues, and Pull requests.
Enable **Device flow** in its settings, install it on the selected repositories,
and set `KAGEHA_GITHUB_CLIENT_ID` in the environment of the WebUI and agent processes.
Restart the backend to expose **Connect with GitHub**. Follow the displayed link,
enter the device code, and approve authorization. Then select repositories and agents.
No callback URL is needed for this device flow.

For expiring GitHub App user tokens, set `KAGEHA_GITHUB_CLIENT_SECRET` in both
processes to enable automatic refresh. Without it, Kageha asks you to reconnect
when the access token expires. Never put the client secret in frontend variables.

GitHub's device flow and registration instructions:
https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app

## Storage and access

Connection data is encrypted in `$KAGEHA_HOME/integrations/github.enc` (default
`~/.kageha`). The encryption key is stored alongside it in an owner-only file;
this protects against accidental plaintext disclosure, not compromise of the
host account. Kageha's local-owner trust model applies: tool grants are connector
restrictions, not isolation from other tools with host filesystem access.

Tokens are never returned by the Settings API or exposed as tool arguments.
The integration uses fixed GitHub API endpoints and does not follow redirects.
Every tool call checks the current agent and repository grants. Disconnect removes
local credentials immediately; revoke the token or application separately in
GitHub settings to remove authorization on GitHub too.

## Agent tools

`github_read` is registered as a core connector tool. Operations:

- `repositories`: list the assigned repositories.
- `contents`: list a directory or read a file; optional `path` and `ref`.
- `issues`, `pulls`: list open and closed items, 50 per page.
- `issue`, `pull`: read a numbered item.
- `pull_files`: read changed files and patches for a pull request.
- `comments`: read issue or pull request conversation comments.

Lists support `page`; file text is limited to 150 KB and marked when truncated.
GitHub's own limits also apply (large files, directory listings, and PR diffs).
Binary files are not parsed. Returned repository content is untrusted data;
agents should cite the returned GitHub links.

## Verification

Run `python -m pytest tests/test_github_integration.py tests/test_tool_packs.py`.
In `src/kageha/webui/frontend`, run the IntegrationsSettings and SettingsDialog
Vitest tests, then `npm run build`. Mocked integration tests cover HTTP routes,
credential secrecy, access enforcement, token refresh, OAuth polling, and disconnect.
A live private-repository smoke test requires the account owner to connect GitHub.
