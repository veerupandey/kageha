import type { OutcomeCheck, RecoveryMode } from "../api/types";

export type RecoverOutcome = (mode: RecoveryMode, ids: string[]) => void;

export function OutcomeChecks({ outcomes, onRecover }: { outcomes: OutcomeCheck[]; onRecover?: RecoverOutcome }) {
  const pending = outcomes.filter(outcome => outcome.status !== "pass");
  const confirmed = outcomes.length - pending.length;
  return <>
    <details className="mt-3 text-xs text-ink">
      <summary className="cursor-pointer font-medium">
        Checks: {confirmed} confirmed · {pending.length} need attention
      </summary>
      <ul className="mt-2 space-y-2" aria-label="Outcome checks">
        {outcomes.map(outcome => (
          <li key={outcome.id} className="flex flex-col items-start gap-0.5 sm:flex-row sm:gap-2">
            <span className="shrink-0 font-medium">
              {{ pass: "Confirmed", fail: "Needs attention", blocked: "Blocked", unresolved: "Needs checking" }[outcome.status]}:
            </span>
            <div className="min-w-0 break-words">
              <span>{outcome.label}</span>
              {outcome.historical ? <span className="ml-1 text-muted">(earlier check)</span> : null}
              {outcome.evidence?.length ? <details className="mt-1 text-muted">
                <summary className="cursor-pointer">Supporting evidence ({outcome.evidence.length})</summary>
                {outcome.evidence.map(receipt => <div key={receipt.id} className="mt-1 break-words">
                  <span>{receipt.label}</span>{" · "}
                  {/^https?:\/\//i.test(receipt.reference)
                    ? <a className="underline" href={receipt.reference} target="_blank" rel="noopener noreferrer">Open source</a>
                    : <span>{receipt.reference}</span>}
                  {receipt.timestamp ? <span> · {new Date(receipt.timestamp * 1000).toLocaleString()}</span> : null}
                  {receipt.detail ? <p className="mt-1 whitespace-pre-wrap">{receipt.detail}</p> : null}
                </div>)}
              </details> : null}
              {onRecover ? <button type="button" className="mt-1 block underline" aria-label={`Check ${outcome.label} again`}
                onClick={() => onRecover("recheck", [outcome.id])}>Check this result again</button> : null}
            </div>
          </li>
        ))}
      </ul>
    </details>
    {pending.length > 0 && onRecover ? <button type="button"
      className="mt-3 rounded-lg bg-accent px-3 py-2 text-xs font-medium text-white"
      onClick={() => onRecover("continue", pending.map(o => o.id))}>Continue unfinished work</button> : null}
  </>;
}
