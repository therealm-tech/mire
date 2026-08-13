/**
 * The API contract, as schemas.
 *
 * Every response is parsed before it reaches a component, so the runtime shape
 * and the static type cannot drift. No business logic lives here or anywhere
 * else in the UI — `mire` decides, the UI shows.
 */

import { z } from 'zod'
import { logger } from './logger'

/**
 * The server injects `<base href>` into `index.html` at serve time, so a
 * relative URL resolves correctly whether we are mounted at `/` or under a
 * notebook proxy prefix. Nothing here ever knows the prefix.
 */
function endpoint(path: string): string {
  return new URL(path, document.baseURI).toString()
}

export const loadIssueSchema = z.object({
  file: z.string(),
  message: z.string(),
  line: z.number().nullable(),
  column: z.number().nullable(),
})

export const profileKindSchema = z.enum(['chat', 'embedding'])

export const profileSummarySchema = z.object({
  name: z.string(),
  kind: profileKindSchema,
  url: z.string(),
  auth: z.string().nullable(),
  source: z.string(),
  hasDecode: z.boolean(),
})

export const profilesResponseSchema = z.object({
  profiles: z.array(profileSummarySchema),
  issues: z.array(loadIssueSchema),
})

/** A live browser login. Never carries a token — that stays on the server. */
export const sessionViewSchema = z.object({
  subject: z.string().optional(),
  scope: z.string().optional(),
  expiresInS: z.number(),
  canRefresh: z.boolean(),
})

export const authDescriptorSchema = z.object({
  name: z.string(),
  kind: z.enum(['anonymous', 'token', 'oidc', 'oidc_browser']),
  needsValue: z.boolean(),
  needsLogin: z.boolean(),
  session: sessionViewSchema.optional(),
  lastError: z.string().optional(),
})

export const loginResponseSchema = z.object({
  authorizationUrl: z.string(),
  redirectUri: z.string(),
  state: z.string(),
})

export const logoutResponseSchema = z.object({
  signedOut: z.boolean(),
})

export const authResponseSchema = z.object({
  providers: z.array(authDescriptorSchema),
  issues: z.array(loadIssueSchema),
})

const usageSchema = z.object({
  promptTokens: z.number().nullable(),
  completionTokens: z.number().nullable(),
  totalTokens: z.number().nullable(),
  raw: z.unknown(),
})

const toolCallSchema = z.object({
  id: z.string().optional(),
  name: z.string(),
  arguments: z.unknown(),
})

export type ToolCall = z.infer<typeof toolCallSchema>

/**
 * One conversation turn, as `Message` on the server.
 *
 * Sent back verbatim on the next call: `mire` holds no conversation of its own,
 * so the whole history travels in the body every time. That is what keeps the
 * `curl` export of turn five a reproduction of turn five rather than of a
 * session that no longer exists.
 */
export interface Message {
  role: 'system' | 'user' | 'assistant' | 'tool'
  content?: string
  /**
   * Handed back in the normalised shape the response decoded to. The server
   * reads either that or the nested wire shape, so this is understood — but the
   * *encoding* of `arguments` is not preserved, which is one more reason a
   * conversation that reached a tool call belongs in agent mode.
   */
  toolCalls?: ToolCall[]
  toolCallId?: string
}

const completionSchema = z.object({
  kind: z.literal('completion'),
  content: z.string().nullable(),
  toolCalls: z.array(toolCallSchema),
  finishReason: z.string().nullable(),
  usage: usageSchema.nullable(),
})

const dimensionsSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('uniform'), value: z.number() }),
  z.object({ kind: z.literal('ragged'), values: z.array(z.number()) }),
  z.object({ kind: z.literal('unknown') }),
])

const histogramSchema = z.object({
  min: z.number(),
  max: z.number(),
  buckets: z.array(z.number()),
})

const vectorSummarySchema = z.object({
  index: z.number(),
  dimensions: z.number(),
  norm: z.number(),
  // A non-finite value serialises as `null`, which is exactly the case
  // `finite: false` is telling you about.
  sample: z.array(z.number().nullable()),
  finite: z.boolean(),
  histogram: histogramSchema,
})

export const checkOutcomeSchema = z.discriminatedUnion('status', [
  z.object({ status: z.literal('pass') }),
  z.object({ status: z.literal('fail'), detail: z.string() }),
  z.object({ status: z.literal('skipped'), reason: z.string() }),
])

const embeddingChecksSchema = z.object({
  count: checkOutcomeSchema,
  dimensions: checkOutcomeSchema,
  finite: checkOutcomeSchema,
  nonZeroNorm: checkOutcomeSchema,
  determinism: checkOutcomeSchema,
})

const embeddingSchema = z.object({
  kind: z.literal('embedding'),
  count: z.number(),
  dimensions: dimensionsSchema,
  encoding: z.enum(['float', 'base64', 'none']),
  usage: usageSchema.nullable(),
  vectors: z.array(vectorSummarySchema),
  full: z.array(z.array(z.number().nullable())).optional(),
  checks: embeddingChecksSchema,
})

export const decodedSchema = z.discriminatedUnion('kind', [completionSchema, embeddingSchema])

export const decodeTraceSchema = z.object({
  matched: z.record(z.string(), z.string()),
  missed: z.record(z.string(), z.array(z.string())),
  issues: z.array(z.object({ field: z.string(), path: z.string(), message: z.string() })),
})

const httpMetaSchema = z.object({
  status: z.number(),
  headers: z.record(z.string(), z.string()),
  latencyMs: z.number(),
  ttftMs: z.number().optional(),
})

/**
 * What the stream itself did, as opposed to what it said.
 *
 * Present only on a streamed response, and its presence is what identifies one:
 * `raw` then holds the final chunk rather than a whole body, because there is no
 * whole body.
 */
export const streamViewSchema = z.object({
  framing: z.enum(['sse', 'ndjson']).optional(),
  chunks: z.number(),
  deltas: z.number(),
  unparsable: z.number(),
  bytes: z.number(),
  terminated: z.boolean(),
  firstChunkMs: z.number().optional(),
})

const responseViewSchema = z.object({
  http: httpMetaSchema,
  bodyText: z.string().optional(),
  raw: z.unknown().nullable(),
  elided: z.boolean(),
  jsonError: z.string().optional(),
  decoded: decodedSchema.optional(),
  decode: decodeTraceSchema,
  stream: streamViewSchema.optional(),
})

export const callOutcomeSchema = z.object({
  profile: z.string(),
  auth: z.string(),
  dryRun: z.boolean(),
  request: z.object({
    method: z.string(),
    url: z.string(),
    headers: z.record(z.string(), z.string()),
    body: z.string(),
  }),
  curl: z.string(),
  response: responseViewSchema.optional(),
  retriedAfterUnauthorized: z.boolean(),
})

const stopOutcomeSchema = z.discriminatedUnion('outcome', [
  z.object({
    outcome: z.literal('stopped'),
    reason: z.discriminatedUnion('predicate', [
      z.object({ predicate: z.literal('noToolCalls') }),
      z.object({ predicate: z.literal('finishReason'), value: z.string() }),
    ]),
  }),
  z.object({ outcome: z.literal('maxIterations'), limit: z.number() }),
  z.object({ outcome: z.literal('deadline'), afterMs: z.number() }),
  z.object({ outcome: z.literal('repeatedCall'), tool: z.string(), atTurn: z.number() }),
  z.object({
    outcome: z.literal('predicateNeverEvaluable'),
    predicate: z.string(),
    turns: z.number(),
  }),
])

const toolInvocationSchema = z.object({
  call: toolCallSchema,
  /** `mcp` means something really happened outside this process. */
  source: z.enum(['simulated', 'mcp']),
  server: z.string().optional(),
  latencyMs: z.number().optional(),
  /** The tool ran and reported a problem. A result, not a failure of the run. */
  reportedError: z.boolean(),
  schemaErrors: z.array(z.string()),
  result: z.string(),
  error: z.string().optional(),
})

const decisionSchema = z.discriminatedUnion('decision', [
  z.object({ decision: z.literal('continue'), tools: z.number() }),
  z.object({ decision: z.literal('stop'), stop: stopOutcomeSchema }),
])

export const turnSchema = z.object({
  index: z.number(),
  call: callOutcomeSchema,
  tools: z.array(toolInvocationSchema),
  decision: decisionSchema,
})

export const traceSchema = z.object({
  profile: z.string(),
  auth: z.string(),
  turns: z.array(turnSchema),
  stop: stopOutcomeSchema,
  durationMs: z.number(),
})

// The server flattens the payload alongside the `event` tag, so the shapes are
// spread rather than intersected — an intersection is not discriminable.
export const agentEventSchema = z.discriminatedUnion('event', [
  z.object({ event: z.literal('turn'), ...turnSchema.shape }),
  z.object({ event: z.literal('done'), ...traceSchema.shape }),
  z.object({ event: z.literal('failed'), code: z.string(), message: z.string() }),
])

/** What `POST /api/call/stream` emits. */
export const streamEventSchema = z.discriminatedUnion('event', [
  z.object({
    event: z.literal('open'),
    status: z.number(),
    headers: z.record(z.string(), z.string()),
  }),
  z.object({ event: z.literal('delta'), text: z.string() }),
  z.object({ event: z.literal('done'), ...callOutcomeSchema.shape }),
  z.object({ event: z.literal('failed'), code: z.string(), message: z.string() }),
])

export const errorBodySchema = z.object({
  code: z.string(),
  message: z.string(),
  detail: z.unknown().optional(),
})

export type LoadIssue = z.infer<typeof loadIssueSchema>
export type ProfileKind = z.infer<typeof profileKindSchema>
export type ProfileSummary = z.infer<typeof profileSummarySchema>
export type ProfilesResponse = z.infer<typeof profilesResponseSchema>
export type AuthDescriptor = z.infer<typeof authDescriptorSchema>
export type AuthResponse = z.infer<typeof authResponseSchema>
export type SessionView = z.infer<typeof sessionViewSchema>
export type LoginResponse = z.infer<typeof loginResponseSchema>
export type Decoded = z.infer<typeof decodedSchema>
export type Completion = Extract<Decoded, { kind: 'completion' }>
export type Embedding = Extract<Decoded, { kind: 'embedding' }>
export type CheckOutcome = z.infer<typeof checkOutcomeSchema>
export type DecodeTrace = z.infer<typeof decodeTraceSchema>
export type CallOutcome = z.infer<typeof callOutcomeSchema>
export type StopOutcome = z.infer<typeof stopOutcomeSchema>
export type ToolInvocation = z.infer<typeof toolInvocationSchema>
export type Turn = z.infer<typeof turnSchema>
export type Trace = z.infer<typeof traceSchema>
export type AgentEvent = z.infer<typeof agentEventSchema>
export type StreamView = z.infer<typeof streamViewSchema>
export type StreamEvent = z.infer<typeof streamEventSchema>
export type ErrorBody = z.infer<typeof errorBodySchema>

/** What a call needs. Mirrors `CallRequest` on the server. */
export interface CallRequest {
  profile: string
  auth: string
  prompt?: string
  /** The conversation so far. Takes precedence over `prompt` on the server. */
  messages?: Message[]
  input?: string[]
  token?: string
  dryRun?: boolean
  includeVectors?: boolean
  repeat?: number
  /** Told to the template as `stream`. `POST /api/call/stream` forces it on. */
  stream?: boolean
}

/** An API failure, carrying the server's structured error when there was one. */
export class ApiError extends Error {
  readonly status: number
  readonly body: ErrorBody

  constructor(status: number, body: ErrorBody) {
    super(body.message)
    this.name = 'ApiError'
    this.status = status
    this.body = body
  }
}

async function request<T>(path: string, schema: z.ZodType<T>, init?: RequestInit): Promise<T> {
  const url = endpoint(path)
  const response = await fetch(url, init)
  const text = await response.text()
  logger.debug('api.response', { url, status: response.status, bytes: text.length })

  let payload: unknown
  try {
    payload = JSON.parse(text)
  } catch {
    throw new ApiError(response.status, {
      code: 'unreadable_response',
      message: `${url} answered ${response.status} with something that is not JSON`,
    })
  }

  if (!response.ok) {
    const parsed = errorBodySchema.safeParse(payload)
    throw new ApiError(
      response.status,
      parsed.success
        ? parsed.data
        : { code: 'unexpected_error', message: `${url} answered ${response.status}` },
    )
  }

  const parsed = schema.safeParse(payload)
  if (!parsed.success) {
    logger.error('api.schema_mismatch', { url, issues: parsed.error.issues })
    throw new ApiError(response.status, {
      code: 'schema_mismatch',
      message: `${url} answered in a shape this UI does not understand`,
      detail: parsed.error.issues,
    })
  }
  return parsed.data
}

export function fetchProfiles(): Promise<ProfilesResponse> {
  return request('api/profiles', profilesResponseSchema)
}

export function fetchAuth(): Promise<AuthResponse> {
  return request('api/auth', authResponseSchema)
}

/**
 * Where the identity provider has to send the browser back.
 *
 * Computed here, in the browser, because this is the only place the public URL
 * is known. Inside a Kubeflow notebook the server binds `127.0.0.1:8787` and has
 * no way to learn that we are being served from
 * `https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`. `document.baseURI`
 * does know — it is the `<base href>` the server injected, resolved against the
 * page's own origin — so the callback follows the browser rather than the socket.
 *
 * `--public-url` on the server overrides this, for a proxy that rewrites paths.
 */
export function callbackUri(): string {
  return endpoint('auth/callback')
}

/**
 * Starts a browser login.
 *
 * `prompt` is the way out of a login that keeps failing instantly: with an
 * established SSO session the identity provider redirects straight back, so a
 * broken attempt repeats itself with nothing to interact with. `login` makes it
 * ask again.
 */
export function startLogin(
  provider: string,
  redirectUri: string,
  prompt?: string,
): Promise<LoginResponse> {
  return request(`api/auth/${encodeURIComponent(provider)}/login`, loginResponseSchema, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(prompt ? { redirectUri, prompt } : { redirectUri }),
  })
}

export function logout(provider: string): Promise<{ signedOut: boolean }> {
  return request(`api/auth/${encodeURIComponent(provider)}/logout`, logoutResponseSchema, {
    method: 'POST',
  })
}

export function call(body: CallRequest): Promise<CallOutcome> {
  return request('api/call', callOutcomeSchema, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
}

/** One agent run. Mirrors `AgentRequest` on the server. */
export interface AgentRequest extends CallRequest {
  maxIterations?: number
}

/**
 * Reads a server-sent event stream from a POST, calling `onEvent` per event.
 *
 * `EventSource` cannot POST, so the stream is read by hand — which is fine,
 * because the framing is three lines and doing it here keeps the request body
 * where it belongs. One reader for both streaming endpoints: a second hand-rolled
 * frame loop is a second place to lose the last chunk of every other message.
 */
async function streamEvents<T>(
  path: string,
  body: unknown,
  schema: z.ZodType<T>,
  onEvent: (event: T) => void,
  signal?: AbortSignal,
): Promise<void> {
  const url = endpoint(path)
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    ...(signal ? { signal } : {}),
  })

  if (!response.ok) {
    const text = await response.text()
    let payload: unknown
    try {
      payload = JSON.parse(text)
    } catch {
      payload = null
    }
    const parsed = errorBodySchema.safeParse(payload)
    throw new ApiError(
      response.status,
      parsed.success
        ? parsed.data
        : { code: 'unexpected_error', message: `${url} answered ${response.status}` },
    )
  }
  if (!response.body) {
    throw new ApiError(response.status, {
      code: 'no_stream',
      message: `${url} answered without a body`,
    })
  }

  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader()
  let buffer = ''

  while (true) {
    const { done, value } = await reader.read()
    if (done) {
      break
    }
    buffer += value

    // Frames are separated by a blank line.
    let boundary = buffer.indexOf('\n\n')
    while (boundary !== -1) {
      dispatch(buffer.slice(0, boundary), schema, onEvent)
      buffer = buffer.slice(boundary + 2)
      boundary = buffer.indexOf('\n\n')
    }
  }
}

function dispatch<T>(frame: string, schema: z.ZodType<T>, onEvent: (event: T) => void): void {
  const data = frame
    .split('\n')
    .filter((line) => line.startsWith('data: '))
    .map((line) => line.slice(6))
    .join('\n')
  if (data.length === 0) {
    return
  }

  let payload: unknown
  try {
    payload = JSON.parse(data)
  } catch {
    logger.warn('stream.unparsable_frame', { frame })
    return
  }

  const parsed = schema.safeParse(payload)
  if (parsed.success) {
    onEvent(parsed.data)
  } else {
    logger.error('stream.schema_mismatch', { issues: parsed.error.issues })
  }
}

/** Runs an agent loop, calling `onEvent` as each turn arrives. */
export function runAgent(
  body: AgentRequest,
  onEvent: (event: AgentEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  return streamEvents('api/agent', body, agentEventSchema, onEvent, signal)
}

/**
 * Runs one call, streamed.
 *
 * The `done` event carries the same outcome `call()` returns, so a caller can
 * ignore every delta and still end up with the full answer — the live text is a
 * bonus, not the only copy.
 */
export function streamCall(
  body: CallRequest,
  onEvent: (event: StreamEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  return streamEvents('api/call/stream', body, streamEventSchema, onEvent, signal)
}
