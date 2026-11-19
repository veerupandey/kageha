/** Routes of the Activity panel's Task_Workspace views (`/api/sessions/<id>/workspace/...`). */

/** How often the plan and tree refresh while a turn runs. */
export const WORKSPACE_POLL_MS = 4000;

export const workspaceBase = (sessionId: string) => `/api/sessions/${encodeURIComponent(sessionId)}/workspace`;

/** URL of a workspace file: opened inline, or downloaded as an attachment. */
export function workspaceFileUrl(sessionId: string, path: string, download = false): string {
  return `${workspaceBase(sessionId)}/file?path=${encodeURIComponent(path)}${download ? "&download=1" : ""}`;
}
