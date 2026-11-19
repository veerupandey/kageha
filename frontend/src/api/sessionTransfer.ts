import { ApiError, api, redirectToLoginIfNeeded } from "./client";

export const MAX_SESSION_ARCHIVE_BYTES = 500 * 1024 * 1024;

export async function downloadSession(sessionId: string): Promise<number> {
  const response = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/export`);
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    redirectToLoginIfNeeded(response.status, data);
    throw new ApiError(String(data.error || "Session export failed"), response.status, data);
  }
  if (response.headers.get("Content-Type")?.split(";")[0] !== "application/zip") {
    throw new Error("The server did not return a session archive");
  }
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement("a");
  link.href = url;
  link.download = `kageha-session-${sessionId}.zip`;
  document.body.appendChild(link);
  try {
    link.click();
  } finally {
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return Number(response.headers.get("X-Kageha-Unavailable-Attachments")) || 0;
}

export async function uploadSession(file: File): Promise<{ session_id: string }> {
  if (file.size > MAX_SESSION_ARCHIVE_BYTES) {
    throw new Error("Session archive exceeds 500 MiB");
  }
  return api("/api/sessions/import", {
    method: "POST", headers: { "Content-Type": "application/zip" }, body: file,
  });
}
