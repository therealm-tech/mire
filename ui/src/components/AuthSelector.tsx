import type { AuthDescriptor, LoadIssue, SessionView } from '../api'
import { Badge, Field } from './primitives'

/** "expires in 4 min", or the blunt truth. */
function expiry(session: SessionView): string {
  if (session.expiresInS <= 0) {
    return session.canRefresh ? 'expired, renews on the next call' : 'expired'
  }
  const minutes = Math.round(session.expiresInS / 60)
  return minutes < 1 ? `expires in ${session.expiresInS}s` : `expires in ${minutes} min`
}

/**
 * Above everything else, on purpose: replaying the same request on every auth
 * mode is the central move of this tool, so switching mode is one click and
 * changes nothing else.
 */
export function AuthSelector({
  providers,
  issues,
  selected,
  token,
  signingIn,
  loginError,
  onSelect,
  onToken,
  onLogin,
  onLogout,
}: {
  providers: AuthDescriptor[]
  issues: LoadIssue[]
  selected: string
  token: string
  signingIn: string | null
  loginError: string | null
  onSelect: (name: string) => void
  onToken: (token: string) => void
  onLogin: (name: string, prompt?: string) => void
  onLogout: (name: string) => void
}) {
  const current = providers.find((provider) => provider.name === selected)

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1">
        {providers.map((provider) => {
          const active = provider.name === selected
          return (
            <button
              key={provider.name}
              type="button"
              aria-pressed={active}
              onClick={() => onSelect(provider.name)}
              className={`rounded border px-2.5 py-1 text-sm ${
                active
                  ? 'border-stone-900 bg-stone-900 text-stone-50 dark:border-stone-100 dark:bg-stone-100 dark:text-stone-900'
                  : 'border-stone-300 hover:bg-stone-100 dark:border-stone-700 dark:hover:bg-stone-800'
              }`}
            >
              {provider.name}
              {provider.kind === provider.name ? null : (
                <span className="ml-1.5 opacity-60">{provider.kind}</span>
              )}
              {provider.needsLogin ? (
                <span
                  role="img"
                  aria-label={provider.session ? 'signed in' : 'not signed in'}
                  className={`ml-1.5 inline-block h-1.5 w-1.5 rounded-full align-middle ${
                    provider.session ? 'bg-emerald-500' : 'bg-stone-400'
                  }`}
                />
              ) : null}
            </button>
          )
        })}
      </div>

      {current?.needsLogin ? (
        <BrowserLogin
          provider={current}
          signingIn={signingIn === current.name}
          error={loginError}
          onLogin={(prompt) => onLogin(current.name, prompt)}
          onLogout={() => onLogout(current.name)}
        />
      ) : null}

      {current?.needsValue ? (
        <Field label="Token (kept in this tab, never stored)">
          <input
            type="password"
            value={token}
            autoComplete="off"
            onChange={(event) => onToken(event.target.value)}
            placeholder="paste the credential"
            className="w-full rounded border border-stone-300 bg-white px-2 py-1 font-mono text-sm dark:border-stone-700 dark:bg-stone-950"
          />
        </Field>
      ) : null}

      {current?.kind === 'anonymous' ? (
        <p className="text-stone-500 text-xs dark:text-stone-400">
          Nothing is sent. A <span className="font-mono">401</span> here means the route is
          protected, and counts as a pass.
        </p>
      ) : null}

      {current?.kind === 'oidc' ? (
        <p className="text-stone-500 text-xs dark:text-stone-400">
          A workload identity: <span className="font-mono">client_credentials</span>, fetched
          without anybody signing in. This is what a pod would send.
        </p>
      ) : null}

      {issues.length > 0 ? (
        <ul className="space-y-1">
          {issues.map((issue) => (
            <li key={`${issue.file}:${issue.message}`} className="text-xs">
              <Badge tone="bad">auth.yaml</Badge>{' '}
              <span className="text-stone-600 dark:text-stone-400">{issue.message}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

/** Sign-in state for one `oidc_browser` provider: who you are, or a button. */
function BrowserLogin({
  provider,
  signingIn,
  error,
  onLogin,
  onLogout,
}: {
  provider: AuthDescriptor
  signingIn: boolean
  error: string | null
  onLogin: (prompt?: string) => void
  onLogout: () => void
}) {
  const session = provider.session
  // The callback records why it failed; `error` is what this tab saw. Either way
  // there is something to say, and something better to offer than the same button.
  const failure = error ?? provider.lastError ?? null

  return (
    <div className="space-y-1.5 rounded border border-stone-200 p-2 dark:border-stone-800">
      <div className="flex flex-wrap items-center gap-2">
        {session ? (
          <>
            <Badge tone="good">signed in</Badge>
            <span className="text-sm">{session.subject ?? 'unknown user'}</span>
            <span className="text-stone-500 text-xs dark:text-stone-400">{expiry(session)}</span>
            <button
              type="button"
              onClick={onLogout}
              className="ml-auto rounded border border-stone-300 px-2 py-1 text-xs dark:border-stone-700"
            >
              Sign out
            </button>
          </>
        ) : (
          <>
            <Badge tone="neutral">not signed in</Badge>
            <button
              type="button"
              disabled={signingIn}
              onClick={() => onLogin()}
              className="ml-auto rounded bg-stone-900 px-2.5 py-1 font-medium text-stone-50 text-xs disabled:opacity-50 dark:bg-stone-100 dark:text-stone-900"
            >
              {signingIn ? 'Waiting for the browser…' : failure ? 'Try again' : 'Sign in'}
            </button>
          </>
        )}
      </div>

      {session?.scope ? (
        <p className="font-mono text-[11px] text-stone-500 dark:text-stone-400">
          granted: {session.scope}
        </p>
      ) : null}

      {signingIn ? (
        <p className="text-stone-500 text-xs dark:text-stone-400">
          A tab opened for the identity provider. If nothing appeared, your browser blocked the
          popup.
        </p>
      ) : null}

      {failure ? (
        <div className="space-y-1">
          <p className="text-xs">
            <Badge tone="bad">sign-in failed</Badge>{' '}
            <span className="text-stone-600 dark:text-stone-400">{failure}</span>
          </p>
          <p className="text-stone-500 text-xs dark:text-stone-400">
            If the window opens and shuts instantly, the identity provider is reusing its own
            session and replaying the same failure.{' '}
            <button
              type="button"
              disabled={signingIn}
              onClick={() => onLogin('login')}
              className="underline underline-offset-2 disabled:opacity-50"
            >
              Sign in again, asking for credentials
            </button>{' '}
            forces it to stop and ask.
          </p>
        </div>
      ) : null}

      <p className="text-stone-500 text-xs dark:text-stone-400">
        The tokens stay on the server and never reach this page. Signing out here does not sign you
        out of the identity provider.
      </p>
    </div>
  )
}
