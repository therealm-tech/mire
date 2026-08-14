import type { AuthDescriptor, LoadIssue, ProfileSummary, SessionView } from '../api'
import { Badge, Button, Field, INPUT_CLASSES } from './primitives'

/** "expires in 4 min", or the blunt truth. */
function expiry(session: SessionView): string {
  if (session.expiresInS <= 0) {
    return session.canRefresh ? 'expired, renews on the next call' : 'expired'
  }
  const minutes = Math.round(session.expiresInS / 60)
  return minutes < 1 ? `expires in ${session.expiresInS}s` : `expires in ${minutes} min`
}

/** The host a profile points at, or `null` from a URL that will not parse. */
export function hostOf(url: string): string | null {
  try {
    return new URL(url).hostname
  } catch {
    return null
  }
}

/**
 * Whether this credential is allowed to go where this profile points.
 *
 * `allowed_hosts` is enforced on the server for every call, so a profile whose
 * own `auth:` excludes its own `url:` fails every time. That is a
 * misconfiguration you want to read, not discover.
 */
export function reaches(provider: AuthDescriptor, profile: ProfileSummary): boolean {
  if (provider.allowedHosts.length === 0) {
    return true
  }
  const host = hostOf(profile.url)
  return host === null || provider.allowedHosts.includes(host)
}

/**
 * Who the model call goes out as.
 *
 * Shown, not chosen: the identity belongs to the profile, in its `auth:` field,
 * next to the URL it authenticates against. Offering a different one here would
 * be offering to run something other than the profile — and the two are meant to
 * be the same thing, so that what you read in the file is what went out. To ask
 * the same endpoint under another identity, copy the profile and change one
 * line; that copy is then a thing you can keep, name and re-run.
 *
 * What is still interactive is what nobody could have put in a file: a
 * credential typed into this tab, and a browser session somebody has to go and
 * fetch.
 */
export function ModelAuth({
  provider,
  profile,
  issues,
  token,
  signingIn,
  loginError,
  onToken,
  onLogin,
  onLogout,
}: {
  provider: AuthDescriptor | undefined
  profile: ProfileSummary | null | undefined
  issues: LoadIssue[]
  token: string
  signingIn: string | null
  loginError: { provider: string; message: string } | null
  onToken: (token: string) => void
  onLogin: (name: string, prompt?: string) => void
  onLogout: (name: string) => void
}) {
  const declared = profile?.auth ?? null

  return (
    <div className="space-y-2">
      {provider === undefined ? (
        <p className="text-xs">
          <Badge tone="bad">{declared ?? 'unknown'}</Badge>{' '}
          <span className="text-muted">
            named by this profile, declared in no <span className="font-mono">auth.yaml</span> entry
          </span>
        </p>
      ) : (
        <div className="space-y-1.5" data-testid="model-auth">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="font-medium text-sm">{provider.name}</span>
            {provider.kind === provider.name ? null : (
              <span className="text-faint text-xs">{provider.kind}</span>
            )}
            {declared === null ? (
              <span className="text-faint text-xs">
                no <span className="font-mono">auth:</span> in this profile
              </span>
            ) : null}
            {profile && !reaches(provider, profile) ? (
              <Badge tone="bad">out of allowed_hosts</Badge>
            ) : null}
          </div>

          {profile && !reaches(provider, profile) ? (
            <p className="text-muted text-xs">
              This credential may only be sent to{' '}
              <span className="font-mono">{provider.allowedHosts.join(', ')}</span>, and the profile
              points at <span className="font-mono">{hostOf(profile.url)}</span>. Every call is
              refused before anything goes out.
            </p>
          ) : null}

          {provider.needsLogin ? (
            <BrowserLogin
              provider={provider}
              signingIn={signingIn === provider.name}
              error={loginError?.provider === provider.name ? loginError.message : null}
              onLogin={(prompt) => onLogin(provider.name, prompt)}
              onLogout={() => onLogout(provider.name)}
            />
          ) : null}

          {provider.needsValue ? (
            <Field label="Token (kept in this tab, never stored)">
              <input
                type="password"
                value={token}
                autoComplete="off"
                onChange={(event) => onToken(event.target.value)}
                placeholder="paste the credential"
                className={`${INPUT_CLASSES} w-full font-mono`}
              />
            </Field>
          ) : null}

          {provider.kind === 'anonymous' ? (
            <p className="text-muted text-xs">
              Nothing is sent. A <span className="font-mono">401</span> here means the route is
              protected, and counts as a pass.
            </p>
          ) : null}

          {provider.kind === 'oidc' ? (
            <p className="text-muted text-xs">
              A workload identity: <span className="font-mono">client_credentials</span>, fetched
              without anybody signing in. This is what a pod would send.
            </p>
          ) : null}
        </div>
      )}

      {issues.length > 0 ? (
        <ul className="space-y-1">
          {issues.map((issue) => (
            <li key={`${issue.file}:${issue.message}`} className="text-xs">
              <Badge tone="bad">auth.yaml</Badge>{' '}
              <span className="text-muted">{issue.message}</span>
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
    <div className="space-y-1.5 rounded border border-line p-2">
      <div className="flex flex-wrap items-center gap-2">
        {session ? (
          <>
            <Badge tone="good">signed in</Badge>
            <span className="text-sm">{session.subject ?? 'unknown user'}</span>
            <span className="text-faint text-xs">{expiry(session)}</span>
            <Button className="ml-auto" onClick={onLogout}>
              Sign out
            </Button>
          </>
        ) : (
          <>
            <Badge tone="neutral">not signed in</Badge>
            <Button
              variant="primary"
              className="ml-auto"
              disabled={signingIn}
              onClick={() => onLogin()}
            >
              {signingIn ? 'Waiting for the browser…' : failure ? 'Try again' : 'Sign in'}
            </Button>
          </>
        )}
      </div>

      {session?.scope ? (
        <p className="font-mono text-[11px] text-faint">granted: {session.scope}</p>
      ) : null}

      {signingIn ? (
        <p className="text-muted text-xs">
          A tab opened for the identity provider. If nothing appeared, your browser blocked the
          popup.
        </p>
      ) : null}

      {failure ? (
        <div className="space-y-1">
          <p className="text-xs">
            <Badge tone="bad">sign-in failed</Badge> <span className="text-muted">{failure}</span>
          </p>
          <p className="text-muted text-xs">
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

      <p className="text-faint text-xs">
        The tokens stay on the server and never reach this page. Signing out here does not sign you
        out of the identity provider.
      </p>
    </div>
  )
}
