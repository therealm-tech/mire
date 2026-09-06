import type { AuthDescriptor, McpDescriptor, ModelSummary } from './api'

/** The host a model points at, or `null` from a URL that will not parse. */
export function hostOf(url: string): string | null {
  try {
    return new URL(url).hostname
  } catch {
    return null
  }
}

/**
 * Whether this credential is allowed to go where this model points.
 *
 * `allowed_hosts` is enforced on the server for every call, so a model whose
 * own `auth:` excludes its own `url:` fails every time. That is a
 * misconfiguration you want to read, not discover.
 */
export function reaches(provider: AuthDescriptor, model: ModelSummary): boolean {
  if (provider.allowedHosts.length === 0) {
    return true
  }
  const host = hostOf(model.url)
  return host === null || provider.allowedHosts.includes(host)
}

/** Something that will stop the next call, and — sometimes — the way out of it. */
export interface Blocker {
  message: string
  /** The provider to sign in to, when signing in is what fixes this. */
  signIn?: string
  /** Set when the fix is a field inside the auth panel, which is otherwise shut. */
  opensAuth?: boolean
  /** Set when the fix is **Attach**, which is a button on the composer. */
  needsUpload?: boolean
}

/** What the next call would do, and whether it can happen at all. */
export interface Preflight {
  /** The endpoint it would call. */
  url: string
  /** The name of the identity it would call as. */
  identity: string
  /** The MCP servers it would set up before the first turn. Empty when none are on. */
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
  model,
  provider,
  providers,
  servers,
  token,
  uploads = 0,
  mcpActive = [],
  mcpDeclared = [],
}: {
  model: ModelSummary
  /** The resolved model identity, `undefined` when the model names one that is not declared. */
  provider: AuthDescriptor | undefined
  providers: AuthDescriptor[]
  servers: McpDescriptor[]
  /** What has been typed into this tab, for a provider that has to be asked. */
  token: string
  /**
   * The servers this run sets up, by id — one per `mcp/` file, at the stage that
   * is picked.
   *
   * Worked out by the caller rather than here, because the MCP block has already
   * worked it out to draw itself and two readings of "which servers is this run
   * about" would be two things that can disagree. Empty on an embedding model,
   * which has no loop to call a tool from: reporting "tool calls answer 409"
   * about a run that makes none would be painting the bar red over a call that
   * is going to go through.
   */
  mcpActive?: string[]
  /**
   * Every server `mcp/` declares, by name, on a run that could reach one.
   *
   * Only to say when a run reaches none of them: nothing is on until somebody
   * switches it on, so "the model is offered no live tool" is the ordinary state
   * and finding that out by reading the traffic afterwards is finding out too
   * late. Empty where there is no loop to call a tool from, which is a run with
   * nothing to say on the subject rather than one that left something out.
   */
  mcpDeclared?: string[]
  /**
   * How many files this tab has attached.
   *
   * Only a blocker against a `requires_upload:` model, and a count rather than
   * a flag because that is what the rule is: the model asks for a file, not for
   * a particular one. Which of them the request actually uses is the template's
   * business, and it is not read here.
   */
  uploads?: number
}): Preflight {
  const blockers: Blocker[] = []
  const notes: string[] = []

  // Said first, because it is the one blocker fixed by a button on the composer
  // rather than by anything in the auth panel — and on a transcriber it is the
  // only thing between an empty run and an answer.
  if (model.requiresUpload && uploads === 0) {
    blockers.push({
      message: `${model.name} needs a file: attach one, since the request is built around it.`,
      needsUpload: true,
    })
  }

  if (provider === undefined) {
    blockers.push({
      message: `This model names ${model.auth ?? 'an identity'}, which no provider in auth/ declares.`,
    })
  } else {
    if (!reaches(provider, model)) {
      blockers.push({
        message: `${provider.id} may only be sent to ${provider.allowedHosts.join(', ')}, and this model points at ${hostOf(model.url) ?? model.url}.`,
      })
    }
    if (provider.needsValue && token.trim().length === 0) {
      blockers.push({
        message: `${provider.id} has no value: paste the credential below, since the server was given no env: or file: to read it from.`,
        opensAuth: true,
      })
    }
    if (provider.needsLogin && !provider.session) {
      blockers.push({
        message: `Nobody is signed in to ${provider.id}.`,
        signIn: provider.id,
      })
    }
  }

  for (const server of servers.filter(({ id }) => mcpActive.includes(id))) {
    const name = server.id
    // The named provider and the ones its header templates read: any of them
    // being a browser flow with no session is a `409` on the first tool call.
    const used = [server.auth, ...server.usesAuth].filter(
      (entry) => entry !== undefined && entry.length > 0,
    )
    for (const entry of new Set(used)) {
      const identity = providers.find((candidate) => candidate.id === entry)
      if (identity?.needsLogin && !identity.session) {
        blockers.push({
          message: `Tool calls to ${name} answer 409 until somebody signs in to ${identity.id}.`,
          signIn: identity.id,
        })
      }
    }
  }

  if (mcpDeclared.length > 0 && mcpActive.length === 0) {
    notes.push(
      `No MCP server in this run: ${mcpDeclared.length} declared in mcp/, none switched on — the model is offered no live tool.`,
    )
  }

  if (!model.hasDecode) {
    notes.push('No decode: block, so nothing will be read out of the answer.')
  }

  return {
    url: model.url,
    identity: provider?.id ?? model.auth ?? 'unknown',
    servers: mcpActive,
    blockers,
    notes,
  }
}
