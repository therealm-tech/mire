import type { AuthDescriptor, McpDescriptor, ProfileSummary } from './api'

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

/** Something that will stop the next call, and — sometimes — the way out of it. */
export interface Blocker {
  message: string
  /** The provider to sign in to, when signing in is what fixes this. */
  signIn?: string
  /** Set when the fix is a field inside the auth panel, which is otherwise shut. */
  opensAuth?: boolean
}

/** What the next call would do, and whether it can happen at all. */
export interface Preflight {
  /** The endpoint it would call. */
  url: string
  /** The name of the identity it would call as. */
  identity: string
  /** The MCP servers it would set up before the first turn. */
  servers: string[]
  /** Each one refuses this call outright — see the message for which and why. */
  blockers: Blocker[]
  /** True of the call, but not fatal to it. */
  notes: string[]
}

/**
 * What the next call would do, worked out before it is made.
 *
 * Every blocker here is a refusal the server has already been shown to make, not
 * a guess this side is hazarding: an undeclared provider or MCP server is a
 * `404` before the stream opens, a missing value is `no_credential`, a missing
 * browser session is `409 not_signed_in`, and a credential outside its
 * `allowed_hosts` is refused before anything goes out. The point is that all of
 * them are knowable *now* — the tool's whole claim is that you can see the
 * signal going in, and "will this even leave the process" is part of that.
 *
 * It says nothing about whether the endpoint is up. That is the question being
 * asked, and answering it here would be answering it by guessing.
 */
export function preflight({
  profile,
  provider,
  providers,
  servers,
  token,
}: {
  profile: ProfileSummary
  /** The resolved model identity, `undefined` when the profile names one that is not declared. */
  provider: AuthDescriptor | undefined
  providers: AuthDescriptor[]
  servers: McpDescriptor[]
  /** What has been typed into this tab, for a provider that has to be asked. */
  token: string
}): Preflight {
  const blockers: Blocker[] = []
  const notes: string[] = []

  if (provider === undefined) {
    blockers.push({
      message: `This profile names ${profile.auth ?? 'an identity'}, which no auth.yaml entry declares.`,
    })
  } else {
    if (!reaches(provider, profile)) {
      blockers.push({
        message: `${provider.name} may only be sent to ${provider.allowedHosts.join(', ')}, and this profile points at ${hostOf(profile.url) ?? profile.url}.`,
      })
    }
    if (provider.needsValue && token.trim().length === 0) {
      blockers.push({
        message: `${provider.name} has no value: paste the credential below, since the server was given no env: or file: to read it from.`,
        opensAuth: true,
      })
    }
    if (provider.needsLogin && !provider.session) {
      blockers.push({
        message: `Nobody is signed in to ${provider.name}.`,
        signIn: provider.name,
      })
    }
  }

  for (const name of profile.mcp) {
    const server = servers.find((candidate) => candidate.name === name)
    if (!server) {
      blockers.push({
        message: `This profile names the MCP server ${name}, which no mcp.yaml entry declares.`,
      })
      continue
    }

    // The named provider and the ones its header templates read: any of them
    // being a browser flow with no session is a `409` on the first tool call.
    const used = [server.auth, ...server.usesAuth].filter(
      (entry) => entry !== undefined && entry.length > 0,
    )
    for (const entry of new Set(used)) {
      const identity = providers.find((candidate) => candidate.name === entry)
      if (identity?.needsLogin && !identity.session) {
        blockers.push({
          message: `Tool calls to ${name} answer 409 until somebody signs in to ${identity.name}.`,
          signIn: identity.name,
        })
      }
    }
  }

  if (!profile.hasDecode) {
    notes.push('No decode: block, so nothing will be read out of the answer.')
  }

  return {
    url: profile.url,
    identity: provider?.name ?? profile.auth ?? 'unknown',
    servers: profile.mcp,
    blockers,
    notes,
  }
}
