import { useAppStore } from "../store";

export function QueuedMessages() {
  const queue = useAppStore((s) => s.sessionId ? s.runs[s.sessionId]?.queue : undefined);
  const sendNowMessage = useAppStore((s) => s.sendNowMessage);
  const steerMessage = useAppStore((s) => s.steerMessage);
  const removeQueuedMessage = useAppStore((s) => s.removeQueuedMessage);
  if (!queue?.length) return null;
  return (
    <div className="mb-2 flex flex-wrap gap-1.5" aria-label="Queued messages">
      {queue.map((message, index) => (
        <span key={index} className="rounded-full bg-accent-soft px-2 py-0.5 text-xs text-accent" title={message.text}>
          Queued · {(message.text || "(files)").slice(0, 48)}
          <button type="button" className="ml-2 font-medium hover:text-ink" title="Apply this queued message after the current tool call" onClick={() => { void steerMessage(message.text, message); }}>Steer</button>
          <button type="button" className="ml-2 font-medium hover:text-ink" title="Stop the current turn and send this queued message now" onClick={() => { void sendNowMessage(message.text, message); }}>Send now</button>
          <button type="button" className="ml-2 hover:text-ink" aria-label={`Remove queued message ${index + 1}`} onClick={() => removeQueuedMessage(index)}>×</button>
        </span>
      ))}
    </div>
  );
}
