import type { BrowserState } from "../api/types";
import { browserProfileLabel } from "../lib/browserPreview";

/** A backend may have several retained attachments. Select the exact identity. */
export function BrowserSessionSelect({ state, disabled, onChange }: {
  state: BrowserState;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const current = state.browserSessions?.find(session => session.selected);
  const backend = state.selectedBackend || state.backend || "chromium";
  if (state.previewOnly || backend === "rust") {
    const profiles = state.privateCanary ? ["private"] : [...new Set(["private", "alternate", ...(state.availableProfiles || []),
      ...(state.browserSessions || []).map(session => session.id)])]
      .filter(profile => !["chrome", "comet", "cdp"].includes(profile));
    const signedIn = ["chrome", "comet", "cdp"].filter(profile => state.selectedProfile === profile
      || state.availableProfiles?.includes(profile) || (!state.privateCanary && profile !== "cdp"));
    return (
      <select aria-label="Browser profile" value={`profile:${state.selectedProfile || "private"}`}
        title={disabled ? "Browser action in progress" : "Switch the browser profile used by the agent"}
        className="min-w-0 max-w-52 truncate rounded-md border border-line bg-surface px-2 py-1 text-[0.65rem] text-muted disabled:opacity-60"
        disabled={disabled} onChange={event => onChange(event.target.value)}>
        <optgroup label="Private browsing">
          {profiles.map(profile => <option key={profile} value={`profile:${profile}`}>
            {profile === "alternate" ? "Alternate private browser"
              : profile === "docker" ? "Docker browser"
                : profile === "machine" ? "Session computer browser" : browserProfileLabel(profile)}
          </option>)}
        </optgroup>
        {signedIn.length > 0 && <optgroup label="Signed-in profiles · share sign-in with the agent">
          {signedIn.map(profile => <option key={profile} value={`profile:${profile}`}>
            {profile === "cdp" ? "Custom CDP profile" : browserProfileLabel(profile)}
          </option>)}
        </optgroup>}
      </select>
    );
  }
  return (
    <select aria-label="Browser profile" value={current ? `session:${current.id}` : backend}
      title={disabled ? "Browser action in progress" : state.lockedBy === "user" ? "Switch browser session" : "Take control and switch browser session"}
      className="min-w-0 max-w-52 truncate rounded-md border border-line bg-surface px-2 py-1 text-[0.65rem] text-muted disabled:cursor-default disabled:opacity-60"
      disabled={disabled} onChange={event => onChange(event.target.value)}>
      {state.browserSessions?.length ? (
        <optgroup label="Open in this chat">
          {state.browserSessions.map((session, index) => (
            <option key={session.id} value={`session:${session.id}`}>
              {browserProfileLabel(session.backend)} · {index + 1}{session.title ? ` · ${session.title}` : ""}{!session.connected ? " (disconnected)" : ""}
            </option>
          ))}
        </optgroup>
      ) : null}
      <optgroup label="Browser choices">
        <option value="chromium">Private browser</option>
        {state.backend === "embedded" && <option value="embedded">Kageha profile</option>}
        <option value="comet">Comet profile</option>
        <option value="chrome">Chrome profile</option>
        {!["chromium", "embedded", "comet", "chrome"].includes(backend) && (
          <option value={backend}>{browserProfileLabel(backend)}</option>
        )}
      </optgroup>
    </select>
  );
}
