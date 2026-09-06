import type { Preflight as PreflightState, Row } from '../preflight'
import { Badge, Button, INPUT_CLASSES } from './primitives'

/**
 * What the next call would do, above the box you would do it from.
 *
 * The tool's claim is that you put a known signal in and look at what comes out.
 * Everything about the second half was already on the page; this is the first
 * half — where it goes, who it goes as, what it will set up first — said before
 * it happens rather than reconstructed from a trace afterwards.
 *
 * One shape says all of it: a badge, what the line is about, what there is to
 * say, and the button that fixes it. An identity that is signed in and one that
 * is not are that same line with a different badge, so the page does not
 * rearrange itself around a sign-in — which is what a panel folded away behind
 * a button, and a red list of sentences replacing it, both did.
 */
export function Preflight({
  state,
  mcpOpen,
  showMcp,
  token,
  signingIn,
  onToken,
  onSignIn,
  onSignOut,
  onOpenMcp,
}: {
  state: PreflightState
  mcpOpen: boolean
  /** False on a run that will not speak to a server — see `usesMcp` in `App`. */
  showMcp: boolean
  /** What has been typed into this tab, for the row that asks for a credential. */
  token: string
  signingIn: string | null
  onToken: (token: string) => void
  onSignIn: (provider: string, prompt?: string) => void
  onSignOut: (provider: string) => void
  onOpenMcp: () => void
}) {
  return (
    <section
      className="rounded-lg border border-line bg-panel px-3 py-2 text-xs"
      aria-label="What the next call will do"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        {/*
          Where the call goes, and nothing about how it is going to turn out: a
          verdict here was a second badge over the top of the lines below, which
          carry it already and say which of them it is about.
        */}
        <span className="min-w-0 truncate font-mono text-muted">{state.url}</span>

        {state.servers.length > 0 ? (
          <span className="text-faint">
            · {state.servers.length} MCP {state.servers.length === 1 ? 'server' : 'servers'} (
            <span className="font-mono">{state.servers.join(', ')}</span>)
          </span>
        ) : null}

        {showMcp ? (
          <span className="ml-auto">
            <Button aria-expanded={mcpOpen} onClick={onOpenMcp}>
              {mcpOpen ? 'Hide MCP' : 'MCP'}
            </Button>
          </span>
        ) : null}
      </div>

      <ul className="mt-2 space-y-1.5">
        {state.rows.map((row) => (
          <li
            key={row.key}
            className="flex flex-wrap items-center gap-2 border-line border-t pt-1.5"
          >
            <Badge tone={row.tone}>{row.label}</Badge>
            {/*
              A provider called `anonymous`, in the state called `anonymous`:
              the badge has already said it, and saying it twice reads as a
              stutter rather than as two facts.
            */}
            {row.subject === row.label ? null : (
              <span className="font-medium text-sm">{row.subject}</span>
            )}
            {/* Empty on a row the badge has already said the whole of. */}
            {row.detail ? (
              <span className={row.blocks && !row.prompts ? 'text-bad' : 'text-faint'}>
                {row.detail}
              </span>
            ) : null}

            {row.prompts ? (
              <input
                type="password"
                value={token}
                autoComplete="off"
                aria-label={`Credential for ${row.subject}`}
                onChange={(event) => onToken(event.target.value)}
                placeholder="paste the credential"
                className={`${INPUT_CLASSES} w-64 max-w-full font-mono`}
              />
            ) : null}

            <Action row={row} signingIn={signingIn} onSignIn={onSignIn} onSignOut={onSignOut} />
          </li>
        ))}
      </ul>

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

/**
 * The button a row ends on, when it has one.
 *
 * A failed sign-in gets two: pressing the same one again usually replays the
 * identity provider's own session and so replays the same failure, and `prompt`
 * is what forces it to stop and ask.
 */
function Action({
  row,
  signingIn,
  onSignIn,
  onSignOut,
}: {
  row: Row
  signingIn: string | null
  onSignIn: (provider: string, prompt?: string) => void
  onSignOut: (provider: string) => void
}) {
  const fix = row.fix
  if (fix === undefined) {
    return null
  }

  if (fix.kind === 'sign-out') {
    return (
      <span className="ml-auto">
        <Button onClick={() => onSignOut(fix.provider)}>Sign out</Button>
      </span>
    )
  }

  const waiting = signingIn === fix.provider

  return (
    <span className="ml-auto flex items-center gap-1">
      {fix.retry ? (
        <Button disabled={signingIn !== null} onClick={() => onSignIn(fix.provider, 'login')}>
          Ask for credentials
        </Button>
      ) : null}
      <Button
        variant="primary"
        disabled={signingIn !== null}
        onClick={() => onSignIn(fix.provider)}
      >
        {waiting ? 'Waiting for the browser…' : fix.retry ? 'Try again' : 'Sign in'}
      </Button>
    </span>
  )
}
