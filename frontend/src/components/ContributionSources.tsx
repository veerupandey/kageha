import type { ContributionSource, DurableJob } from "./DurableJobs";

function destination(source: ContributionSource, jobId: string): string | null {
  if (source.navigation?.status !== "linked" || !source.navigation.href) return null;
  const href = source.navigation.href;
  if (source.navigation.kind === "file") {
    const prefix = `/api/jobs/${encodeURIComponent(jobId)}/artifacts/`;
    const index = href.slice(prefix.length);
    return href.startsWith(prefix) && index !== "" && Number.isSafeInteger(Number(index)) && Number(index) >= 0 && String(Number(index)) === index ? href : null;
  }
  try {
    const url = new URL(href);
    return source.navigation.kind === "web" && ["https:", "http:"].includes(url.protocol) && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

export function ContributionSources({ job }: { job: DurableJob }) {
  const sources = (job.result.contribution || job.progress?.contribution)?.sources || [];
  if (!sources.length) return null;
  return <section aria-label="Contribution sources">
    <h4 className="mb-2 text-xs font-semibold text-muted">Sources</h4>
    <ul className="space-y-2">{sources.map((source, index) => {
      const href = destination(source, job.id);
      const web = href && source.navigation?.kind === "web";
      const detail = web ? new URL(href).hostname : source.navigation?.kind === "file" ? "Published file" : "Reference";
      return <li key={index} className="rounded-lg border border-line p-3">
        {href ? <a className="block break-words text-sm font-medium text-accent hover:underline" href={href} target="_blank" rel="noopener noreferrer" aria-label={`Open source: ${source.title} (new tab)`}>{source.title}<span aria-hidden="true" className="ml-1">↗</span></a> : <p className="break-words text-sm font-medium text-ink">{source.title}</p>}
        <p className="mt-1 break-words text-xs text-muted">{href ? detail : source.navigation?.status === "unpublished" ? "File not published · link unavailable" : "Link unavailable"}{source.locator ? ` · ${source.locator}` : ""}</p>
      </li>;
    })}</ul>
    <p className="mt-2 text-xs text-muted">References supplied by this specialist. A link does not mean the source or claim has been independently verified.</p>
  </section>;
}
