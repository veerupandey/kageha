import { useEffect, useState } from "react";
import { api } from "../api/client";
import { useAppStore } from "../store";

type Reminder = { id: string; title: string; body: string; origin_session_id: string; session_id?: string; status: string; triage?: { priority?: string } };

export function ReminderNotifications() {
  const [items, setItems] = useState<Reminder[]>([]);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState("");
  const openSession = useAppStore((s) => s.openSession);
  useEffect(() => {
    let active = true;
    const refresh = async () => {
      try {
        const data = await api<{ notifications?: Reminder[] }>("/api/notifications");
        if (active) setItems(data.notifications || []);
      } catch { /* Keep already-loaded reminders during reconnect. */ }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 8000);
    window.addEventListener("focus", refresh);
    return () => { active = false; window.clearInterval(timer); window.removeEventListener("focus", refresh); };
  }, []);
  if (!items.length) return null;
  const acknowledge = async (item: Reminder, view: boolean) => {
    try {
      if (view) await openSession(item.status === "success" ? item.origin_session_id : item.session_id || item.origin_session_id);
      await api(`/api/notifications/${item.id}/read`, { method: "POST" });
      setItems((current) => current.filter((row) => row.id !== item.id));
      setError("");
    } catch { setError("Could not open or dismiss this reminder. Please retry."); }
  };
  return <aside aria-label="Reminder notifications" className="fixed bottom-4 right-4 z-50 max-w-sm rounded-xl border border-line bg-surface p-3 text-ink shadow-lg">
    <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="text-sm font-medium">Reminders · {items.length} unread</button>
    {open && <div className="mt-2 max-h-80 space-y-3 overflow-auto">
      {items.map((item) => <article key={item.id}>
        <p className="text-sm font-medium">{item.title}</p>
        {item.triage?.priority && item.triage.priority !== "routine" && <span className="mt-1 inline-block rounded-full border border-line px-2 py-0.5 text-[0.65rem] capitalize text-muted">{item.triage.priority}</span>}
        <p className="mt-1 whitespace-pre-wrap text-xs text-muted">{item.body}</p>
        <div className="mt-2 flex gap-3 text-xs">
          <button type="button" onClick={() => void acknowledge(item, true)}>Open chat</button>
          <button type="button" onClick={() => void acknowledge(item, false)}>Dismiss</button>
        </div>
      </article>)}
    </div>}
    {error && <p role="alert" className="mt-2 text-xs">{error}</p>}
  </aside>;
}
