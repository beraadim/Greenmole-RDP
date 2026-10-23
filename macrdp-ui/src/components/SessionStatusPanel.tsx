import { useSessionStatus } from "../hooks/useSessionStatus";
import {
  cleanupEvidenceLabel, sessionOutcomeLabel, sessionPhaseLabel,
  type CleanupEvidence, type SessionSummary,
} from "../lib/sessionStatus";

function evidenceColor(evidence: CleanupEvidence) {
  if (evidence === "complete" || evidence === "not_required") return "text-green";
  if (evidence === "failed" || evidence === "pending") return "text-orange";
  return "text-text-muted";
}
function CleanupDetails({ session }: { session: SessionSummary }) {
  const rows: [string, CleanupEvidence][] = [
    ["Screen capture", session.cleanup.capture], ["Desktop restoration", session.cleanup.display],
    ["Native video encoder", session.cleanup.native_encoder], ["Wake assertions", session.cleanup.wake],
    ["Private session protection", session.cleanup.private_guard],
  ];
  return (
    <div className="mt-2 grid grid-cols-1 sm:grid-cols-2 gap-x-5 gap-y-1 text-[11px]">
      {rows.map(([label, evidence]) => (
        <div className="flex justify-between gap-3" key={label}>
          <span className="text-text-muted">{label}</span>
          <span className={evidenceColor(evidence)}>{cleanupEvidenceLabel(evidence)}</span>
        </div>
      ))}
      {session.cleanup.observation_limited && <p className="text-orange sm:col-span-2">Some lifecycle evidence was lost or exceeded its observation limit; cleanup cannot be fully confirmed.</p>}
      <div className="text-text-muted sm:col-span-2 mt-1">
        {session.cleanup.display_disposition === "removed" ? "Virtual display removed." :
          session.cleanup.display_disposition === "retained_disabled" ? "Virtual display retained and disabled." :
            "Virtual display disposition not confirmed."}
        {" "}Confirmation requires lifecycle completion evidence; a disconnect alone is insufficient.
      </div>
    </div>
  );
}
function SessionDetail({ session, current }: { session: SessionSummary; current: boolean }) {
  return (
    <div>
      <div className="flex flex-wrap justify-between items-center gap-2">
        <span className="text-xs font-medium">{current ? sessionPhaseLabel(session.phase) : "Last session"}</span>
        <span className="text-[11px] text-text-muted">
          {new Date(session.started_at_ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}
        </span>
      </div>
      <p className="mt-1 text-[11px] text-text-muted">
        Host transport: {session.host_transport} · Encoder: {session.encoder}
      </p>
      {!current && <p className="mt-1 text-[11px]">{sessionOutcomeLabel(session.outcome)}</p>}
      {(!current || session.phase === "restoring_desktop") && (
        <details className="mt-2">
          <summary className={`cursor-pointer text-[11px] ${session.cleanup.verified ? "text-green" : "text-text-muted"}`}>
            {session.cleanup.verified ? "Cleanup confirmed" : "Cleanup not fully confirmed"}
          </summary>
          <CleanupDetails session={session} />
        </details>
      )}
    </div>
  );
}
export default function SessionStatusPanel({ showHistory = false }: { showHistory?: boolean }) {
  const { snapshot, unavailable } = useSessionStatus();
  const displayed = snapshot.current ?? snapshot.recent[0];
  return (
    <section className="bg-card rounded-[8px] p-3" aria-label="Session status">
      <h2 className="text-[11px] font-medium text-text-muted mb-2">{showHistory ? "Recent session outcomes and cleanup" : "Session status"}</h2>
      {unavailable && <p role="status" className="text-[11px] text-orange mb-2">Session evidence is temporarily unavailable.</p>}
      {displayed ? <SessionDetail session={displayed} current={!!snapshot.current} /> :
        <p className="text-xs text-text-muted">No session observed since opening this app.</p>}
      {snapshot.current && <p className="text-[11px] text-text-muted mt-2">Host streaming does not confirm delivery or presentation in the browser.</p>}
      {showHistory && snapshot.recent.length > (snapshot.current ? 0 : 1) && (
        <div className="border-t border-border-subtle mt-3 pt-2 flex flex-col gap-2">
          {snapshot.recent.slice(snapshot.current ? 0 : 1).map(session => (
            <details key={session.sequence}>
              <summary className="cursor-pointer flex flex-wrap gap-x-3 text-[11px]">
                <span>{new Date(session.started_at_ms).toLocaleString()}</span>
                <span>{sessionOutcomeLabel(session.outcome)}</span>
                <span className={session.cleanup.verified ? "text-green" : "text-text-muted"}>
                  {session.cleanup.verified ? "Cleanup confirmed" : "Cleanup not fully confirmed"}
                </span>
              </summary>
              <CleanupDetails session={session} />
            </details>
          ))}
        </div>
      )}
      {showHistory && <p className="text-[11px] text-text-muted mt-3">Up to 20 sessions from this app run. Included in the sanitized diagnostics export.</p>}
    </section>
  );
}
