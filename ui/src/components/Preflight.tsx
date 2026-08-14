import type { Preflight as PreflightState } from '../preflight'
import { Badge, Button } from './primitives'

/**
 * What the next call would do, above the box you would do it from.
 *
 * The tool's claim is that you put a known signal in and look at what comes out.
 * Everything about the second half was already on the page; this is the first
 * half — where it goes, who it goes as, what it will set up first — said before
 * it happens rather than reconstructed from a trace afterwards.
 *
 * When something would refuse the call it says so here, with the button that
 * fixes it. The alternative is what this replaces: press **Send**, read a `409`,
 * work out which of two identities it was about, and go and find the row.
 */
export function Preflight({
  state,
  authOpen,
  signingIn,
  onSignIn,
  onOpenAuth,
}: {
  state: PreflightState
  authOpen: boolean
  signingIn: string | null
  onSignIn: (provider: string) => void
  onOpenAuth: () => void
}) {
  const blocked = state.blockers.length > 0

  return (
    <section
      className={`rounded-lg border px-3 py-2 text-xs ${
        blocked ? 'border-bad/40 bg-bad-soft' : 'border-line bg-panel'
      }`}
      aria-label="What the next call will do"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <Badge tone={blocked ? 'bad' : 'good'}>{blocked ? 'blocked' : 'ready'}</Badge>

        <span className="min-w-0 truncate font-mono text-muted">{state.url}</span>

        <span className="text-faint">
          as <span className="font-medium text-muted">{state.identity}</span>
        </span>

        {state.servers.length > 0 ? (
          <span className="text-faint">
            · {state.servers.length} MCP {state.servers.length === 1 ? 'server' : 'servers'} (
            <span className="font-mono">{state.servers.join(', ')}</span>)
          </span>
        ) : null}

        {/*
          The details are one click away rather than permanently open: they are a
          thing you read once and then stop reading, and they were costing the
          composer the top of the screen every time.
        */}
        <Button className="ml-auto" aria-expanded={authOpen} onClick={onOpenAuth}>
          {authOpen ? 'Hide auth' : 'Auth'}
        </Button>
      </div>

      {state.blockers.length > 0 ? (
        <ul className="mt-2 space-y-1.5">
          {state.blockers.map((blocker) => (
            <li key={blocker.message} className="flex flex-wrap items-center gap-2">
              <span className="text-bad">{blocker.message}</span>
              {blocker.signIn === undefined ? null : (
                <Button
                  variant="primary"
                  disabled={signingIn !== null}
                  onClick={() => {
                    // `blocker.signIn` is checked above; the closure needs it again.
                    if (blocker.signIn) {
                      onSignIn(blocker.signIn)
                    }
                  }}
                >
                  {signingIn === blocker.signIn
                    ? 'Waiting for the browser…'
                    : `Sign in to ${blocker.signIn}`}
                </Button>
              )}
            </li>
          ))}
        </ul>
      ) : null}

      {state.notes.length > 0 ? (
        <ul className="mt-1.5 space-y-0.5">
          {state.notes.map((note) => (
            <li key={note} className="text-faint">
              {note}
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  )
}
