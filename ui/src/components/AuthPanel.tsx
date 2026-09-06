import type { AuthResponse, ModelSummary } from '../api'
import { ModelAuth } from './ModelAuth'
import { Panel } from './primitives'

/**
 * Who the model call goes out as.
 *
 * One of the two identity questions a run asks, and the only one answered here:
 * the model's comes from the model's `auth:`, a server's from its own entry in
 * `mcp/`, and neither follows the other. The second is asked on the card of the
 * server it is about, in the MCP block, because that is where the sign-in it
 * needs is worth pressing.
 *
 * What is here is the detail — the one-line answer, and anything that would
 * refuse the call, is in the preflight bar above, which is also what opens this.
 */
export function AuthPanel({
  auth,
  model,
  provider,
  token,
  signingIn,
  loginError,
  onToken,
  onLogin,
  onLogout,
}: {
  auth: AuthResponse
  model: ModelSummary | undefined
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
      <ModelAuth
        provider={provider}
        model={model}
        issues={auth.issues}
        token={token}
        signingIn={signingIn}
        loginError={loginError}
        onToken={onToken}
        onLogin={onLogin}
        onLogout={onLogout}
      />
    </Panel>
  )
}
