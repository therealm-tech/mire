import type { AuthResponse, McpResponse, ProfileSummary } from '../api'
import { McpAuth } from './McpAuth'
import { ModelAuth } from './ModelAuth'
import { Panel } from './primitives'

/**
 * Who this run goes out as, in full.
 *
 * Two questions rather than one, and they are answered in two files: the model's
 * identity comes from the profile's `auth:`, a server's from its own entry in
 * `mcp.yaml`, and neither follows the other. What is here is the detail — the
 * one-line answer, and anything that would refuse the call, is in the preflight
 * bar above, which is also what opens this.
 */
export function AuthPanel({
  auth,
  mcp,
  profile,
  provider,
  token,
  signingIn,
  loginError,
  onToken,
  onLogin,
  onLogout,
}: {
  auth: AuthResponse
  mcp: McpResponse
  profile: ProfileSummary | undefined
  provider: AuthResponse['providers'][number] | undefined
  token: string
  signingIn: string | null
  loginError: { provider: string; message: string } | null
  onToken: (token: string) => void
  onLogin: (name: string, prompt?: string) => void
  onLogout: (name: string) => void
}) {
  return (
    <Panel title="Auth">
      <div className="space-y-3">
        <section className="space-y-2">
          <h3 className="font-semibold text-muted text-xs">Model endpoint</h3>
          <ModelAuth
            provider={provider}
            profile={profile}
            issues={auth.issues}
            token={token}
            signingIn={signingIn}
            loginError={loginError}
            onToken={onToken}
            onLogin={onLogin}
            onLogout={onLogout}
          />
        </section>

        {profile && profile.mcp.length > 0 ? (
          <section className="space-y-2 border-line border-t pt-3">
            <h3 className="font-semibold text-muted text-xs">MCP servers</h3>
            <McpAuth
              names={profile.mcp}
              servers={mcp.servers}
              providers={auth.providers}
              signingIn={signingIn}
              loginError={loginError}
              onLogin={onLogin}
              onLogout={onLogout}
            />
          </section>
        ) : null}
      </div>
    </Panel>
  )
}
