import { useEffect, useState } from "react";
import { Icon } from "../lib/icons";

export function VoiceStatus({ recording, transcribing, onFinish, onCancel }: {
  recording: boolean; transcribing: boolean; onFinish: () => void; onCancel: () => void;
}) {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    setSeconds(0);
    if (!recording) return;
    const start = Date.now();
    const timer = window.setInterval(() => setSeconds(Math.floor((Date.now() - start) / 1000)), 1000);
    return () => clearInterval(timer);
  }, [recording]);
  if (!recording && !transcribing) return null;
  return <div className="ka-voice-status">
    <Icon.Mic size={18} />
    <div role="status"><strong>{recording ? "Listening" : "Transcribing…"}</strong><span>{recording ? `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")} · Review your words before sending` : "Your transcript will appear in the message box."}</span></div>
    {recording && <><button type="button" onClick={onCancel}>Cancel</button><button type="button" className="ka-voice-finish" onClick={onFinish}>Finish</button></>}
  </div>;
}
