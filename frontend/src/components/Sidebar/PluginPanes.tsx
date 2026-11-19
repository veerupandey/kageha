type PaneBlock =
  | { type: "text"; text: string }
  | { type: "progress"; label: string; value: number; total: number }
  | { type: "table"; columns: string[]; rows: string[][] }
  | { type: "modules"; items: { name: string; state: "pending" | "active" | "done" | "needs-human" | "error"; detail: string }[] }
  | { type: "links"; items: { label: string; url: string }[] };

export interface PluginPane { mod: string; title: string; blocks: PaneBlock[]; stale: boolean }

function safeLink(value: string): boolean {
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) && !!url.hostname && !url.username && !url.password
      && !Array.from(value).some((char) => /\s/.test(char) || char.charCodeAt(0) < 32);
  } catch { return false; }
}

const stateColors = {
  pending: "bg-muted/20", active: "bg-accent", done: "bg-green-600",
  "needs-human": "bg-amber-500", error: "bg-red-600",
};

export function PluginPanes({ panes }: { panes: PluginPane[] }) {
  return <div className="space-y-3">
    {panes.map((pane) => <section key={pane.mod} aria-label={pane.title} className="space-y-3 rounded border border-line p-3">
      <div><h4 className="font-semibold text-ink">{pane.title}</h4><p className="text-xs text-muted">{pane.mod}</p></div>
      {pane.stale && <p role="status" className="text-amber-600">Last valid view; refresh failed or is pending.</p>}
      {pane.blocks.map((block, index) => {
        switch (block.type) {
          case "text": return <p key={index} className="whitespace-pre-wrap break-words">{block.text}</p>;
          case "progress": return <div key={index}>
            <p>{block.label}: {block.value}/{block.total}</p>
            <progress aria-label={block.label} value={block.value} max={block.total} className="w-full accent-accent" />
          </div>;
          case "table": return <div key={index} className="max-h-80 overflow-auto"><table className="w-full text-left text-xs">
            <thead><tr>{block.columns.map((column, cell) => <th key={cell} scope="col" className="p-2">{column}</th>)}</tr></thead>
            <tbody>{block.rows.map((row, number) => <tr key={number} className="border-t border-line">{row.map((cell, column) => <td key={column} className="whitespace-pre-wrap break-words p-2">{cell}</td>)}</tr>)}</tbody>
          </table></div>;
          case "modules": return <ul key={index} className="max-h-80 space-y-2 overflow-auto">{block.items.map((item, number) => <li key={number} className="rounded border border-line p-2">
            <p className="flex items-center gap-2"><span aria-hidden="true" className={`h-2 w-2 shrink-0 rounded-full ${stateColors[item.state]}`} /><span className="break-all">{item.name}</span><span className="text-xs text-muted">{item.state}</span></p>
            {item.detail && <p className="whitespace-pre-wrap break-words text-xs text-muted">{item.detail}</p>}
          </li>)}</ul>;
          case "links": return <ul key={index}>{block.items.map((item, number) => <li key={number}>{safeLink(item.url)
            ? <a href={item.url} target="_blank" rel="noopener noreferrer" className="text-accent underline">{item.label}</a>
            : <span>{item.label} (invalid link)</span>}</li>)}</ul>;
        }
      })}
    </section>)}
  </div>;
}
