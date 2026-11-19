import { useMemo, useState } from "react";
import { Icon } from "../../../lib/icons";
import { useCopy, useDocText } from "./useDocContent";

function parseDelimitedText(text: string): string[][] {
  if (!text) return [];
  const delim = text.split("\n")[0]?.includes("\t") ? "\t" : ",";
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let inQuotes = false;

  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    const next = text[i + 1];
    if (c === '"') {
      if (inQuotes && next === '"') { cell += '"'; i++; }
      else inQuotes = !inQuotes;
    } else if (c === delim && !inQuotes) {
      row.push(cell.trim());
      cell = "";
    } else if ((c === "\r" || c === "\n") && !inQuotes) {
      if (c === "\r" && next === "\n") i++;
      row.push(cell.trim());
      if (row.some((x) => x !== "")) rows.push(row);
      row = [];
      cell = "";
    } else {
      cell += c;
    }
  }
  if (cell || row.length > 0) {
    row.push(cell.trim());
    if (row.some((x) => x !== "")) rows.push(row);
  }
  return rows;
}

function getColumnLetter(colIndex: number): string {
  let temp = colIndex;
  let letter = "";
  while (temp >= 0) {
    letter = String.fromCharCode((temp % 26) + 65) + letter;
    temp = Math.floor(temp / 26) - 1;
  }
  return letter;
}

export function TableViewer({ url, initialText = "", name: _name }: { url: string; initialText?: string; name: string }) {
  const { text, loading } = useDocText(url, initialText, "Error loading table data.");
  const [query, setQuery] = useState("");
  const [viewMode, setViewMode] = useState<"table" | "raw">("table");
  const { copied, copy } = useCopy();

  const rawRows = useMemo(() => parseDelimitedText(text), [text]);
  const headers = useMemo(() => rawRows[0] || [], [rawRows]);
  const dataRows = useMemo(() => rawRows.slice(1), [rawRows]);

  const filteredRows = useMemo(() => {
    if (!query.trim()) return dataRows;
    const q = query.toLowerCase();
    return dataRows.filter((r) => r.some((c) => c.toLowerCase().includes(q)));
  }, [dataRows, query]);

  if (loading) {
    return (
      <div className="flex h-full w-full items-center justify-center text-sm text-muted">
        <Icon.Loader className="mr-2 animate-spin text-accent" size={18} />
        Loading spreadsheet data…
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[var(--color-surface)]">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b border-line bg-[var(--color-canvas)] px-4 py-2.5 text-xs">
        <div className="flex items-center gap-3">
          <div className="inline-flex rounded-lg border border-line bg-[var(--color-surface)] p-0.5 shadow-xs">
            <button
              type="button"
              className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1 font-medium transition-colors ${viewMode === "table" ? "bg-accent/15 text-accent font-semibold" : "text-muted hover:text-ink"}`}
              onClick={() => setViewMode("table")}
            >
              <Icon.Table size={14} />
              <span>Table</span>
            </button>
            <button
              type="button"
              className={`rounded-md px-3 py-1 font-medium transition-colors ${viewMode === "raw" ? "bg-accent/15 text-accent font-semibold" : "text-muted hover:text-ink"}`}
              onClick={() => setViewMode("raw")}
            >
              Raw CSV
            </button>
          </div>
          <span className="text-[0.72rem] text-muted">
            {dataRows.length.toLocaleString()} rows · {headers.length} columns{query.trim() ? ` · (${filteredRows.length} matching)` : ""}
          </span>
        </div>

        <div className="flex items-center gap-2">
          {viewMode === "table" && (
            <div className="relative">
              <Icon.Search size={14} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-muted" />
              <input
                type="text"
                placeholder="Filter rows…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                className="h-7 w-44 rounded-lg border border-line bg-[var(--color-surface)] pl-8 pr-3 text-xs text-ink placeholder:text-faint focus:border-accent focus:outline-none focus:ring-1 focus:ring-accent"
              />
            </div>
          )}

          <button
            type="button"
            onClick={() => copy(text)}
            className="inline-flex items-center gap-1.5 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-ink transition-colors hover:bg-line/40"
            title="Copy CSV"
          >
            {copied ? <Icon.Check size={14} className="text-emerald-500" /> : <Icon.Copy size={14} />}
            <span>{copied ? "Copied" : "Copy"}</span>
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {viewMode === "table" ? (
          <div className="inline-block min-w-full align-middle">
            <table className="min-w-full border-collapse text-left font-mono text-[0.75rem]">
              <thead className="sticky top-0 z-10 border-b border-line bg-[var(--color-surface)] shadow-xs">
                <tr>
                  <th className="w-12 border-r border-line bg-[var(--color-canvas)] px-2 py-2 text-center text-[0.65rem] font-semibold text-muted">#</th>
                  {headers.map((h, i) => (
                    <th key={i} className="border-r border-line px-3 py-2 font-semibold text-ink whitespace-nowrap">
                      <span className="mr-1 text-[0.62rem] text-faint">{getColumnLetter(i)}:</span>
                      {h || `Column ${i + 1}`}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody className="divide-y divide-line/60">
                {filteredRows.map((row, rIdx) => (
                  <tr key={rIdx} className="transition-colors hover:bg-accent/5 odd:bg-transparent even:bg-[var(--color-canvas)]/40">
                    <td className="w-12 border-r border-line bg-[var(--color-canvas)] px-2 py-1.5 text-center text-[0.65rem] text-muted select-none">{rIdx + 1}</td>
                    {headers.map((_, cIdx) => (
                      <td key={cIdx} className="max-w-md border-r border-line/40 px-3 py-1.5 text-ink/90 truncate whitespace-nowrap" title={row[cIdx] || ""}>
                        {row[cIdx] || ""}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="p-4">
            <pre className="rounded-xl border border-line bg-[#0d1117] p-4 font-mono text-xs text-[#c9d1d9] whitespace-pre-wrap select-text">{text}</pre>
          </div>
        )}
      </div>
    </div>
  );
}
