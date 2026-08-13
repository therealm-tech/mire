import type { AuthDescriptor, McpDescriptor } from '../api'
import { Badge } from './primitives'

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
 * Who the tool calls go out as — which is not who the model call goes out as.
 *
 * A separate question from the one above it, and answered in a separate file:
 * the model's identity comes from the profile's `auth:`, a server's from its own
 * entry in `mcp.yaml`, and neither follows the other. Only the servers this
 * profile actually names are listed.
 *
 * The one thing worth acting on is a browser provider: with no session the call
 * answers `409 not_signed_in` and **nothing is sent**, which is a far better
 * outcome than a confusing `401` but still needs somebody to go and sign in —
 * and with one, the row is what says who the tool calls go out as, so it is also
 * where you drop that identity again. Both buttons are here, on the row that
 * needs them: the provider a server reaches for is often not the one the profile
 * authenticates the model with, so there is nowhere else they could go.
 */
export function McpAuth({
  names,
  servers,
  providers,
  signingIn,
  loginError,
  onLogin,
  onLogout,
}: {
  names: string[]
  servers: McpDescriptor[]
  providers: AuthDescriptor[]
  signingIn: string | null
  loginError: { provider: string; message: string } | null
  onLogin: (name: string) => void
  onLogout: (name: string) => void
}) {
  return (
    <ul className="space-y-1.5">
      {names.map((name) => {
        const server = servers.find((candidate) => candidate.name === name)
        if (!server) {
          return (
            <li key={name} className="text-xs">
              <Badge tone="bad">{name}</Badge>{' '}
              <span className="text-stone-600 dark:text-stone-400">
                named by this profile, declared in no <span className="font-mono">mcp.yaml</span>{' '}
                entry
              </span>
            </li>
          )
        }

        const used = identities(server)
        // Every browser provider this server reaches for. Without a session each
        // one is a `409` on the first tool call; with one it is a name somebody
        // signed in as, and may want to stop being.
        const human = used
          .map(({ name: provider }) => providers.find((entry) => entry.name === provider))
          .filter((entry) => entry !== undefined)
          .filter((entry) => entry.needsLogin)
        const awaited = human.filter((entry) => !entry.session)

        return (
          <li
            key={name}
            className="rounded border border-stone-200 p-2 dark:border-stone-800"
            data-testid={`mcp-${name}`}
          >
            <div
              className="flex flex-wrap items-center gap-1.5"
              data-testid={`mcp-${name}-identities`}
            >
              <span className="font-medium text-sm">{name}</span>
              {used.length === 0 ? (
                <Badge tone="neutral">anonymous</Badge>
              ) : (
                used.map(({ name: provider, templated }) => (
                  <span key={provider} className="inline-flex items-center gap-1">
                    <Badge tone="neutral">{provider}</Badge>
                    {templated ? (
                      <span className="text-[11px] text-stone-500 dark:text-stone-400">
                        in a header template
                      </span>
                    ) : null}
                  </span>
                ))
              )}
              {awaited.length > 0 ? <Badge tone="warn">not signed in</Badge> : null}
            </div>

            <span className="mt-0.5 block truncate font-mono text-[11px] text-stone-500 dark:text-stone-400">
              {server.url}
            </span>

            {human.map((entry) => {
              const failure =
                loginError?.provider === entry.name ? loginError.message : (entry.lastError ?? null)

              // Signed in: the row already names the provider, so all this adds
              // is who that turned out to be, and the way back out.
              if (entry.session) {
                return (
                  <div key={entry.name} className="mt-1 flex flex-wrap items-center gap-2">
                    <Badge tone="good">signed in</Badge>
                    <span className="text-xs">{entry.session.subject ?? 'unknown user'}</span>
                    <button
                      type="button"
                      onClick={() => onLogout(entry.name)}
                      className="ml-auto rounded border border-stone-300 px-2 py-1 text-xs dark:border-stone-700"
                    >
                      Sign out of {entry.name}
                    </button>
                  </div>
                )
              }

              return (
                <div key={entry.name} className="mt-1 space-y-1">
                  <p className="text-stone-500 text-xs dark:text-stone-400">
                    A tool call answers <span className="font-mono">409 not_signed_in</span> and
                    sends nothing until somebody signs in to{' '}
                    <span className="font-medium">{entry.name}</span>.
                  </p>
                  <div className="flex flex-wrap items-center gap-2">
                    <button
                      type="button"
                      disabled={signingIn !== null}
                      onClick={() => onLogin(entry.name)}
                      className="rounded bg-stone-900 px-2.5 py-1 font-medium text-stone-50 text-xs disabled:opacity-50 dark:bg-stone-100 dark:text-stone-900"
                    >
                      {signingIn === entry.name
                        ? 'Waiting for the browser…'
                        : failure
                          ? `Try ${entry.name} again`
                          : `Sign in to ${entry.name}`}
                    </button>
                  </div>
                  {failure ? (
                    <p className="text-xs">
                      <Badge tone="bad">sign-in failed</Badge>{' '}
                      <span className="text-stone-600 dark:text-stone-400">{failure}</span>
                    </p>
                  ) : null}
                </div>
              )
            })}
          </li>
        )
      })}
    </ul>
  )
}
