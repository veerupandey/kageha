import { useEffect, useState } from "react";
import { api } from "../../api/client";

interface Entry { name: string; description: string; source?: string | null }
interface Catalog { status: string; plugins: Entry[] }

export function PluginCatalog({ installed, installing = [], busy, onPreview }: {
  installed: string[]; installing?: string[]; busy: boolean; onPreview: (name: string) => void;
}) {
  const [data, setData] = useState<Catalog>();
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    setError(""); setData(undefined);
    void api<Catalog>(`/api/plugins/catalog${revision ? "?refresh=1" : ""}`)
      .then((result) => { if (active) setData(result); })
      .catch((e) => { if (active) setError(e instanceof Error ? e.message : String(e)); });
    return () => { active = false; };
  }, [revision]);
  const entries = (data?.plugins ?? []).filter((entry) =>
    `${entry.name} ${entry.description}`.toLowerCase().includes(query.toLowerCase()));
  return <section aria-label="Plugin catalog" className="space-y-3 rounded border border-line p-3">
    <div className="flex items-center justify-between gap-2">
      <h3 className="text-sm font-semibold">Plugin catalog</h3>
      <button type="button" disabled={!data && !error} onClick={() => setRevision((value) => value + 1)}>Refresh catalog</button>
    </div>
    <p className="text-xs text-muted">Review each package before installing it. Catalog entries do not imply that every component is supported or connected.</p>
    <input aria-label="Search plugins" placeholder="Search plugins" value={query} onChange={(e) => setQuery(e.target.value)} className="w-full rounded border border-line bg-canvas px-2 py-1 text-sm" />
    {!data && !error && <p role="status" className="text-sm text-muted">Loading catalog…</p>}
    {error && <p role="alert">{error}</p>}
    {data && data.status !== "available" && <p role="status" className="text-sm text-muted">Plugin catalog is {data.status.replaceAll("_", " ")}.</p>}
    {data && data.status === "available" && entries.length === 0 && <p className="text-sm text-muted">No matching plugins.</p>}
    <div className="max-h-96 space-y-2 overflow-auto">
      {entries.map((entry) => <article key={entry.name} className="space-y-1 rounded border border-line p-3 text-sm">
        <div className="flex items-center justify-between gap-3">
          {entry.source ? <a href={entry.source} target="_blank" rel="noreferrer" className="font-medium text-ink">{entry.name}</a> : <span className="font-medium text-ink">{entry.name}</span>}
          <button type="button" disabled={busy || installed.includes(entry.name) || installing.includes(entry.name)} aria-label={`Review ${entry.name}`} onClick={() => onPreview(entry.name)} className="rounded bg-accent px-3 py-1 text-white disabled:opacity-50">
            {installed.includes(entry.name) ? "Installed" : installing.includes(entry.name) ? "In progress" : "Review"}
          </button>
        </div>
        {entry.description && <p>{entry.description}</p>}
      </article>)}
    </div>
  </section>;
}
