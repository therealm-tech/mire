import { Badge } from './primitives'

/**
 * `mire` itself could not run the call.
 *
 * Not the endpoint failing — that is a result, and it goes in the traffic with a
 * status next to it. This is the one case where there is no result at all: a
 * profile that would not resolve, a credential that could not be fetched, a
 * connection that was never made.
 *
 * One component because it is one kind of event. Chat rendered it inline and
 * embedding rendered it as a panel with a different sentence on it, so the same
 * failure looked like two different things depending on which profile you had
 * selected.
 */
export function Failure({ error }: { error: { code: string; message: string; detail?: unknown } }) {
  return (
    <div className="rounded border border-bad/40 bg-bad-soft p-2" role="alert">
      <p className="flex flex-wrap items-baseline gap-2 text-sm">
        <Badge tone="bad">{error.code}</Badge>
        <span>{error.message}</span>
      </p>
      {error.detail === undefined ? null : (
        <pre className="mt-2 overflow-x-auto rounded bg-well p-2 font-mono text-xs">
          {JSON.stringify(error.detail, null, 2)}
        </pre>
      )}
    </div>
  )
}
