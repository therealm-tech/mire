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
  mcpOpen,
  showMcp,
  signingIn,
  onSignIn,
  onOpenAuth,
  onOpenMcp,
}: {
  state: PreflightState
  authOpen: boolean
  mcpOpen: boolean
  /** False on a run that will not speak to a server — see `usesMcp` in `App`. */
  showMcp: boolean
  signingIn: string | null
  onSignIn: (provider: string) => void
  onOpenAuth: () => void
  onOpenMcp: () => void
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
          composer the top of the screen every time. Two questions, so two
          buttons — who the call goes out as, and which servers it sets up first.
        */}
        <span className="ml-auto flex items-center gap-1">
          {showMcp ? (
            <Button aria-expanded={mcpOpen} onClick={onOpenMcp}>
              {mcpOpen ? 'Hide MCP' : 'MCP'}
            </Button>
          ) : null}
          <Button aria-expanded={authOpen} onClick={onOpenAuth}>
            {authOpen ? 'Hide auth' : 'Auth'}
          </Button>
        </span>
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
