/**
 * OS-level notification mirror for peer alerts.
 *
 * Toasts alone are invisible if the tab isn't focused/visible. This adds a
 * real `Notification` when the tab is backgrounded, without ever prompting
 * for permission until the first genuine alert fires — an unsolicited
 * permission prompt on page load is exactly the kind of thing that trains
 * users to reflexively deny everything.
 */

type ShowToast = (text: string, opts?: { actionLabel?: string; actionId?: string }) => void;

let permissionRequested = false;

function canNotify(): boolean {
  return typeof window !== "undefined" && "Notification" in window;
}

async function ensurePermission(): Promise<NotificationPermission | null> {
  if (!canNotify()) return null;
  if (Notification.permission !== "default") return Notification.permission;
  if (permissionRequested) return Notification.permission;
  permissionRequested = true;
  try {
    return await Notification.requestPermission();
  } catch {
    return null;
  }
}

/** Show a toast, and mirror it to a native notification when backgrounded. */
export function notifyPeerAlert(showToast: ShowToast, text: string): void {
  showToast(text, { actionLabel: "Review", actionId: "open_agent_network" });

  if (!canNotify() || document.visibilityState === "visible") return;

  void ensurePermission().then((permission) => {
    if (permission !== "granted") return;
    try {
      const notification = new Notification("Kageha Agent Network", { body: text, tag: "kageha-peer-alert" });
      notification.onclick = () => {
        window.focus();
        notification.close();
        window.dispatchEvent(new CustomEvent("kageha:open-agent-network"));
      };
    } catch {
      /* Some embedded/desktop webviews restrict Notification even when the
       * API is present; toast already covered the in-tab case. */
    }
  });
}
