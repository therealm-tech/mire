import type { AuthDescriptor, McpDescriptor, ModelSummary, SessionView, ValueSource } from './api'
import type { Tone } from './components/primitives'

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

/** "expires in 4 min", or the blunt truth. */
function expiry(session: SessionView): string {
  if (session.expiresInS <= 0) {
    return session.canRefresh ? 'expired, renews on the next call' : 'expired'
  }
  const minutes = Math.round(session.expiresInS / 60)
  return minutes < 1 ? `expires in ${session.expiresInS}s` : `expires in ${minutes} min`
}

/** "env: MODEL_TOKEN", from the source the server named. */
function named(source: ValueSource): string {
  return `${source.from}: ${source.name}`
}

/** What a button on the row would do. */
export type Fix =
  | { kind: 'sign-in'; provider: string; retry: boolean }
  | { kind: 'sign-out'; provider: string }

/**
 * One line of the bar: a badge, what it is about, what there is to say about it,
 * and — sometimes — the way out.
 *
 * The same shape whether the news is good or bad, which is the point. A run
 * whose identity is signed in and one whose identity is not are the same line
 * with a different badge, so watching one become the other does not move
 * anything on the page.
 */
export interface Row {
  /**
   * What this row is about, stable across every state it can be in.
   *
   * Not the badge, which is exactly what changes: a row re-keyed on the news it
   * carries is a row React unmounts on the first keystroke, taking the caret
   * with it the moment `no value` turns into `in this tab`.
   */
  key: string
  /** The badge. */
  label: string
  tone: Tone
  /** Who or what the line is about: a provider, a model, a server. */
  subject: string
  detail: string
  /** True when this row is why the call cannot go out. */
  blocks: boolean
  /** The button, when a button is what fixes it. */
  fix?: Fix
  /**
   * True on the row that carries the credential field.
   *
   * Set for the whole of a provider the UI has to be asked for, filled or not:
   * an input that vanished on the first keystroke would take the caret with it.
   * The badge is what changes when something is typed.
   */
  prompts?: boolean
}

/** What the next call would do, and whether it can happen at all. */
export interface Preflight {
  /** The endpoint it would call. */
  url: string
  /** Everything there is to say about this call, one line each. */
  rows: Row[]
  /** True of the call, but not fatal to it. */
  notes: string[]
}

/**
 * A browser identity, in whichever of its three states it is in.
 *
 * The same line whether the model names it or a server does — one identity is
 * one thing to be signed in to, and which of the two wanted it changes neither
 * the state nor the button. `key` is the only difference: what the row is about,
 * so that a state changing under it does not re-key it.
 */
function browser(provider: AuthDescriptor, key: string): Row {
  const session = provider.session
  if (session) {
    const scope = session.scope ? ` · ${session.scope}` : ''
    return {
      key,
      label: 'signed in',
      tone: 'good',
      subject: provider.id,
      detail: `${session.subject ?? 'unknown user'} · ${expiry(session)}${scope}`,
      blocks: false,
      fix: { kind: 'sign-out', provider: provider.id },
    }
  }
  // The callback records why it failed in a tab the UI does not control, so this
  // is how the reason gets home — and it is worth saying, because the same
  // button pressed again usually replays the same failure.
  if (provider.lastError) {
    return {
      key,
      label: 'sign-in failed',
      tone: 'bad',
      subject: provider.id,
      detail: provider.lastError,
      blocks: true,
      fix: { kind: 'sign-in', provider: provider.id, retry: true },
    }
  }
  return {
    key,
    label: 'not signed in',
    tone: 'bad',
    subject: provider.id,
    detail: '',
    blocks: true,
    fix: { kind: 'sign-in', provider: provider.id, retry: false },
  }
}

/**
 * The identity line: who the model call goes out as, and what state that is in.
 *
 * The badge is the sentence, so what follows it is only ever what the badge
 * cannot hold: a name, a path, a host, a countdown. `anonymous` explained that
 * nothing is sent and a `401` is a pass — true, and said again on every screen
 * of every session until it read as furniture. It is in the docs, where a thing
 * you learn once belongs.
 *
 * Always exactly one, because a call always has exactly one identity — that is
 * what lets the line be read rather than looked for. The order below is the
 * order the answers stop mattering in: a credential that may not be sent
 * anywhere near this model is not worth signing in to.
 */
function identity(model: ModelSummary, provider: AuthDescriptor | undefined, token: string): Row {
  if (provider === undefined) {
    return {
      key: 'identity',
      label: 'undeclared',
      tone: 'bad',
      subject: model.auth ?? 'unknown',
      detail: '',
      blocks: true,
    }
  }

  if (!reaches(provider, model)) {
    return {
      key: 'identity',
      label: 'out of allowed_hosts',
      tone: 'bad',
      subject: provider.id,
      // The host it may go to. The one it would have gone to is the URL on the
      // line above, so the mismatch is read rather than spelled out.
      detail: `allowed_hosts: ${provider.allowedHosts.join(', ')}`,
      blocks: true,
    }
  }

  if (provider.needsLogin) {
    return browser(provider, 'identity')
  }

  if (provider.needsValue) {
    const empty = token.trim().length === 0
    return {
      key: 'identity',
      label: empty ? 'no value' : 'in this tab',
      tone: empty ? 'bad' : 'good',
      subject: provider.id,
      detail: '',
      blocks: empty,
      prompts: true,
    }
  }

  if (provider.kind === 'anonymous') {
    // A model with no `auth:` resolves here, and the two are worth telling
    // apart: an identity somebody chose, or a field nobody filled in.
    return {
      key: 'identity',
      label: 'anonymous',
      tone: 'neutral',
      subject: provider.id,
      detail: model.auth === null ? 'no auth: in this model' : '',
      blocks: false,
    }
  }

  if (provider.kind === 'oidc') {
    const from = provider.valueSource
    return {
      key: 'identity',
      label: 'workload',
      tone: 'neutral',
      subject: provider.id,
      detail: from ? named(from) : '',
      blocks: false,
    }
  }

  const from = provider.valueSource
  return {
    key: 'identity',
    label: 'token',
    tone: 'neutral',
    subject: provider.id,
    detail: from ? named(from) : '',
    blocks: false,
  }
}

/**
 * What the next call would do, worked out before it is made.
 *
 * Who the call goes out as and what it would set up first — never what it
 * carries. A `requires_upload:` model with nothing attached is a real refusal,
 * but the file that fixes it is picked two buttons along on the composer, and a
 * line about it here would be a line about the request in the middle of the
 * lines about the identity.
 *
 * Every blocking row here is a refusal the server has already been shown to
 * make, not a guess this side is hazarding: an undeclared provider or MCP server
 * is a `404` before the stream opens, a missing value is `no_credential`, a
 * missing browser session is `409 not_signed_in`, and a credential outside its
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
  mcpActive = [],
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
}): Preflight {
  const notes: string[] = []
  const mine = identity(model, provider, token)

  // The browser identities this run's servers need — an identity rather than a
  // server per entry, because one sign-in is what fetches all of them and the
  // same line three times over is three times the noise for one button. Which
  // server wanted it is not said: it is the same identity either way, and the
  // card in the block below is where a server's own business is answered.
  //
  // Signed in or not, because both are worth reading. A line that vanished on
  // the way in would be an answer you can only have while it is still wrong.
  const wanted = new Set<string>()
  for (const server of servers.filter(({ id }) => mcpActive.includes(id))) {
    // The named provider and the ones its header templates read: any of them
    // being a browser flow with no session is a `409` on the first tool call.
    const used = [server.auth, ...server.usesAuth].filter(
      (entry) => entry !== undefined && entry.length > 0,
    )
    for (const entry of new Set(used)) {
      const candidate = providers.find((provider) => provider.id === entry)
      if (candidate?.needsLogin) {
        wanted.add(candidate.id)
      }
    }
  }

  // The model's own identity is already a line, and a server naming that same
  // one has nothing to add to it.
  const rows: Row[] = [mine]
  for (const id of wanted) {
    const candidate = providers.find((provider) => provider.id === id)
    if (candidate && id !== mine.subject) {
      rows.push(browser(candidate, `mcp:${id}`))
    }
  }

  if (!model.hasDecode) {
    notes.push('No decode: block, so nothing will be read out of the answer.')
  }

  return {
    url: model.url,
    rows,
    notes,
  }
}
