import type { AuthDescriptor, McpDescriptor } from '../api'
import { group, pick, type ServerGroup } from '../mcp'
import { Badge, Button, Panel } from './primitives'

/**
 * Which providers a server authenticates with, `auth:` first.
 *
 * `usesAuth` carries the ones its header templates read. A server can have both
 * — the named provider is applied last and wins a name collision — and one can
 * appear in both lists, which is why the second is filtered against the first.
 */
export function identities(server: McpDescriptor): { name: string; templated: boolean }[] {
  const named = server.auth ? [{ name: server.auth, templated: false }] : []
  const templated = server.usesAuth
    .filter((name) => name !== server.auth)
    .map((name) => ({ name, templated: true }))
  return [...named, ...templated]
}

/**
 * Every server `mcp/` declares, and what this run does with each of them.
 *
 * One block rather than two halves of two others. A server used to be split
 * across the page — a checkbox in the composer said whether the run reached it,
 * and a row inside the auth panel said who it reached it as — and the two
 * questions are about the same thing: this server, in this run. The switch, the
 * stage, the address, the identity and the sign-in that identity needs are all
 * on one card now.
 *
 * `mcp/` says which servers exist; this says which of them the next run reaches,
 * and **nothing is on until somebody says so**. A tool call here really runs, on
 * somebody's real server, so a run reaching one is a deliberate act rather than
 * the consequence of a file being in a directory — and the questions that come
 * up are the ones a switch answers: does the model get there without the search
 * tool, is this server the thing that has been failing for ten minutes, what
 * does the loop do when the tool it wants is not there.
 *
 * A server left off is not an idle one. The run never discovers it, never lists
 * it, never signs in to it, and its tools are not offered to the model — so what
 * comes back is what the endpoint does without it.
 */
export function McpPanel({
  servers,
  providers,
  on,
  stages,
  disabled,
  signingIn,
  loginError,
  onToggle,
  onStage,
  onLogin,
  onLogout,
}: {
  /** Every declared server, one entry per stage, in registry order. */
  servers: McpDescriptor[]
  providers: AuthDescriptor[]
  /** The files switched on. Names nothing declares are ignored, not shown. */
  on: string[]
  /** The stage last picked, per server name. See `mcpStages` in `App`. */
  stages: Record<string, string>
  disabled: boolean
  signingIn: string | null
  loginError: { provider: string; message: string } | null
  onToggle: (name: string, on: boolean) => void
  onStage: (name: string, stage: string) => void
  onLogin: (name: string) => void
  onLogout: (name: string) => void
}) {
  const groups = group(servers)

  return (
    <Panel title="MCP servers">
      <ul className="space-y-1.5">
        {groups.map((entry) => (
          <Server
            key={entry.name}
            entry={entry}
            active={pick(entry, stages)}
            on={on.includes(entry.name)}
            providers={providers}
            disabled={disabled}
            signingIn={signingIn}
            loginError={loginError}
            onToggle={onToggle}
            onStage={onStage}
            onLogin={onLogin}
            onLogout={onLogout}
          />
        ))}
      </ul>
    </Panel>
  )
}

/** One `mcp/` file: what the run reaches, and who it reaches it as. */
function Server({
  entry,
  active,
  on,
  providers,
  disabled,
  signingIn,
  loginError,
  onToggle,
  onStage,
  onLogin,
  onLogout,
}: {
  entry: ServerGroup
  /** The stage this run would use, which is what the card describes. */
  active: McpDescriptor
  on: boolean
  providers: AuthDescriptor[]
  disabled: boolean
  signingIn: string | null
  loginError: { provider: string; message: string } | null
  onToggle: (name: string, on: boolean) => void
  onStage: (name: string, stage: string) => void
  onLogin: (name: string) => void
  onLogout: (name: string) => void
}) {
  const used = identities(active)
  // Every browser provider this server reaches for. Without a session each one
  // is a `409` on the first tool call; with one it is a name somebody signed in
  // as, and may want to stop being.
  const human = used
    .map(({ name }) => providers.find((provider) => provider.id === name))
    .filter((provider) => provider !== undefined)
    .filter((provider) => provider.needsLogin)
  const awaited = human.filter((provider) => !provider.session)

  return (
    <li className="rounded border border-line p-2" data-testid={`mcp-${entry.name}`}>
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div
            className="flex flex-wrap items-center gap-1.5"
            data-testid={`mcp-${entry.name}-identities`}
          >
            <span className="font-medium text-sm">{entry.name}</span>
            {used.length === 0 ? (
              <Badge tone="neutral">anonymous</Badge>
            ) : (
              used.map(({ name, templated }) => (
                <span key={name} className="inline-flex items-center gap-1">
                  <Badge tone="neutral">{name}</Badge>
                  {templated ? (
                    <span className="text-[11px] text-faint">in a header template</span>
                  ) : null}
                </span>
              ))
            )}
            {on && awaited.length > 0 ? <Badge tone="warn">not signed in</Badge> : null}
          </div>

          <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">
            {active.url}
          </span>
        </div>

        <Switch
          label={entry.name}
          on={on}
          disabled={disabled}
          onChange={(next) => onToggle(entry.name, next)}
        />
      </div>

      {active.stage === undefined ? null : (
        // Disabled while the server is out of the run, because a stage is which
        // reading of the file *this run* uses: there is no run for it to be
        // about until the switch is back on, and the pick is remembered anyway.
        <fieldset className="mt-1.5">
          <legend className="sr-only">Stages of {entry.name}</legend>
          <div className="flex flex-wrap gap-1">
            {entry.entries.map((server) => (
              <Button
                key={server.id}
                variant={server.id === active.id ? 'primary' : 'ghost'}
                aria-pressed={server.id === active.id}
                disabled={disabled || !on}
                onClick={() => {
                  if (server.stage !== undefined) {
                    onStage(entry.name, server.stage)
                  }
                }}
                className="font-mono"
              >
                {server.isDefault ? (
                  <span aria-hidden="true" className="mr-1 opacity-60">
                    •
                  </span>
                ) : null}
                {server.stage}
                {server.isDefault ? (
                  <>
                    {' '}
                    <span className="sr-only">(default)</span>
                  </>
                ) : null}
              </Button>
            ))}
          </div>
        </fieldset>
      )}

      {on
        ? human.map((provider) => {
            const failure =
              loginError?.provider === provider.id
                ? loginError.message
                : (provider.lastError ?? null)

            // Signed in: the row already names the provider, so all this adds is
            // who that turned out to be, and the way back out.
            if (provider.session) {
              return (
                <div key={provider.id} className="mt-1.5 flex flex-wrap items-center gap-2">
                  <Badge tone="good">signed in</Badge>
                  <span className="text-xs">{provider.session.subject ?? 'unknown user'}</span>
                  <Button className="ml-auto" onClick={() => onLogout(provider.id)}>
                    Sign out of {provider.id}
                  </Button>
                </div>
              )
            }

            // Not signed in: the badge on the row above has said so, and what
            // a `409 not_signed_in` is does not need repeating on every card of
            // every run. All this adds is the button that fetches the session.
            return (
              <div key={provider.id} className="mt-1.5 space-y-1">
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    variant="primary"
                    disabled={signingIn !== null}
                    onClick={() => onLogin(provider.id)}
                  >
                    {signingIn === provider.id
                      ? 'Waiting for the browser…'
                      : failure
                        ? `Try ${provider.id} again`
                        : `Sign in to ${provider.id}`}
                  </Button>
                </div>
                {failure ? (
                  <p className="text-xs">
                    <Badge tone="bad">sign-in failed</Badge>{' '}
                    <span className="text-muted">{failure}</span>
                  </p>
                ) : null}
              </div>
            )
          })
        : null}
    </li>
  )
}

/**
 * In this run, or out of it.
 *
 * A `role="switch"` checkbox rather than a plain box: the answer is not "is this
 * one of the servers I mean" — `mcp/` said that — but "is it on for what I am
 * about to send", which is what a switch reads as. The input itself carries the
 * state and the keyboard; the track beside it is what is drawn, which is why the
 * focus ring is put on the track rather than on a control nobody can see.
 */
function Switch({
  label,
  on,
  disabled,
  onChange,
}: {
  label: string
  on: boolean
  disabled: boolean
  onChange: (on: boolean) => void
}) {
  return (
    <label className="relative inline-flex shrink-0 cursor-pointer items-center">
      <input
        type="checkbox"
        role="switch"
        aria-label={label}
        checked={on}
        // Both, from the one value: a checkbox already exposes its state, but
        // `role="switch"` makes `aria-checked` required, and a checker cannot
        // see that the native attribute is answering for it.
        aria-checked={on}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        className="peer sr-only"
      />
      {/*
        `relative` on the track, not just on the label: the thumb is an `after:`
        of this span, and without it the browser anchors it to the label's box —
        which is a pixel taller than the track's inside and leaves the thumb
        sitting low. Positioned here it is inset by 1px against a 16px inner
        height, so the two gaps are equal by construction rather than by
        eyeballing an offset.
      */}
      <span className="relative block h-[1.125rem] w-8 rounded-full border border-line-strong bg-well transition-colors after:absolute after:top-px after:left-px after:h-[0.875rem] after:w-[0.875rem] after:rounded-full after:bg-faint after:transition-transform peer-checked:border-brand peer-checked:bg-brand peer-checked:after:translate-x-[0.875rem] peer-checked:after:bg-on-brand peer-disabled:opacity-50 peer-focus-visible:outline-2 peer-focus-visible:outline-brand peer-focus-visible:outline-offset-2" />
    </label>
  )
}
