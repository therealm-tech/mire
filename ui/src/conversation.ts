/**
 * The conversation, and the traffic underneath it.
 *
 * Two views of the same run, deliberately separated. The **timeline** is what a
 * person reads: questions, answers, and the tool calls a run made on the way.
 * The **exchanges** are what went on a wire: one entry per model call and one
 * per tool invocation, each carrying its request, its decode and its response.
 *
 * The wire history is derived from the timeline rather than stored beside it, so
 * the two cannot drift: what you see is what the next request will carry. No
 * business logic lives here — `mire` decides, this only rearranges.
 */

import type {
  CallOutcome,
  McpExchange,
  Message,
  StopOutcome,
  ToolInvocation,
  Trace,
  Turn,
} from './api'
import type { Tone } from './components/primitives'

/**
 * A stable key for an item, since position is not one — turns get removed, and
 * a run appends to the timeline while it is being read.
 */
let counter = 0
function nextId(prefix: string): string {
  counter += 1
  return `${prefix}-${counter}`
}

/** A turn that will go on the wire, verbatim, on the next request. */
export interface MessageItem {
  kind: 'message'
  id: string
  message: Message
}

/**
 * What one turn of a run did between the question and the answer.
 *
 * Never sent anywhere: the loop answered these tools itself, and replaying the
 * calls without their results is how you get a `400` from a healthy endpoint.
 */
export interface ActivityItem {
  kind: 'activity'
  id: string
  turn: number
  /** The model call that asked for these, so the row can point at its card. */
  call: string | null
  /**
   * The exchanges themselves, not a copy of what is in them.
   *
   * A summary row and the card it summarises are the same event written twice,
   * and holding the exchange is what lets the row say *which* card — the reason
   * a run stops being two lists you read against each other by eye.
   */
  tools: ToolExchange[]
}

/** How a run ended, parked where it happened in the transcript. */
export interface VerdictItem {
  kind: 'verdict'
  id: string
  stop: StopOutcome
  turns: number
  durationMs: number
}

export type ChatItem = MessageItem | ActivityItem | VerdictItem

export function messageItem(message: Message): MessageItem {
  return { kind: 'message', id: nextId('message'), message }
}

export function activityItem(turn: number, exchanges: Exchange[]): ActivityItem {
  return {
    kind: 'activity',
    id: nextId('activity'),
    turn,
    call: exchanges.find((exchange) => exchange.kind === 'model')?.id ?? null,
    tools: exchanges.filter((exchange) => exchange.kind === 'tool'),
  }
}

export function verdictItem(trace: Trace): VerdictItem {
  return {
    kind: 'verdict',
    id: nextId('verdict'),
    stop: trace.stop,
    turns: trace.turns.length,
    durationMs: trace.durationMs,
  }
}

/**
 * The `messages` array the next request will carry.
 *
 * Everything else in the timeline is commentary on how an answer was reached,
 * and `mire` rebuilds that half itself on every run.
 */
export function wireMessages(timeline: ChatItem[]): Message[] {
  return timeline.flatMap((item) => (item.kind === 'message' ? [item.message] : []))
}

/** Position of a message among messages, which is what a person counts. */
export function messagePositions(timeline: ChatItem[]): Map<string, number> {
  const positions = new Map<string, number>()
  for (const item of timeline) {
    if (item.kind === 'message') {
      positions.set(item.id, positions.size + 1)
    }
  }
  return positions
}

/** One call to a model endpoint, request through decode through response. */
export interface ModelExchange {
  kind: 'model'
  id: string
  /** The turn it belongs to, or `null` for a call made outside a loop. */
  turn: number | null
  outcome: CallOutcome
}

/** One tool invocation. `source: 'mcp'` means it really left the process. */
export interface ToolExchange {
  kind: 'tool'
  id: string
  turn: number | null
  invocation: ToolInvocation
}

/**
 * One JSON-RPC round trip with an MCP server: the protocol underneath a tool.
 *
 * Separate from [`ToolExchange`] because they answer different questions. A tool
 * exchange says what the model asked for and what the loop fed back; this says
 * what actually left the process — which is the only place a `401` from the
 * server, a lost session or a refused handshake is visible at all.
 */
export interface ProtocolExchange {
  kind: 'protocol'
  id: string
  /** `null` for the setup traffic, which happens before any turn. */
  turn: number | null
  exchange: McpExchange
}

export type Exchange = ModelExchange | ToolExchange | ProtocolExchange

export function callExchange(outcome: CallOutcome): ModelExchange {
  return { kind: 'model', id: nextId('model'), turn: null, outcome }
}

/**
 * The MCP traffic listing the tools cost, before the loop began.
 *
 * `turn: null` because there was no turn yet — and a run that died here has no
 * turns at all, which is exactly when this is the only thing to read.
 */
export function setupExchanges(mcp: McpExchange[]): ProtocolExchange[] {
  return mcp.map((exchange) => ({
    kind: 'protocol',
    id: nextId('protocol'),
    turn: null,
    exchange,
  }))
}

/**
 * Everything one turn put on a wire, in the order it happened: the model call
 * that asked for the tools, the JSON-RPC that answered it, then the results as
 * the loop fed them back.
 *
 * The raw exchanges come before the digested ones on purpose. Reading downwards
 * is then the actual sequence of events — the model asked for this, that went to
 * the server, and this is what the next request carried.
 */
export function turnExchanges(turn: Turn): Exchange[] {
  const model: ModelExchange = {
    kind: 'model',
    id: nextId('model'),
    turn: turn.index,
    outcome: turn.call,
  }
  const protocol: ProtocolExchange[] = turn.mcp.map((exchange) => ({
    kind: 'protocol',
    id: nextId('protocol'),
    turn: turn.index,
    exchange,
  }))
  const tools: ToolExchange[] = turn.tools.map((invocation) => ({
    kind: 'tool',
    id: nextId('tool'),
    turn: turn.index,
    invocation,
  }))
  return [model, ...protocol, ...tools]
}

/**
 * A `401` is only bad news when you were expecting to be let in. Asked
 * anonymously, it is the answer you wanted, and it shows up green.
 */
export function statusTone(status: number, expectUnauthorized: boolean): Tone {
  if (status >= 200 && status < 300) return 'good'
  if (expectUnauthorized && (status === 401 || status === 403)) return 'good'
  if (status >= 400) return 'bad'
  return 'warn'
}

/**
 * Whether this exchange is one of the ones you are looking for.
 *
 * The failures, in the four shapes they come in: a status the endpoint should
 * not have answered, a stream that stopped rather than ended, a protocol round
 * trip that never landed, and a tool that failed, reported a problem, or was
 * called with arguments its own schema refuses. `expectUnauthorized` is carried
 * through because a `401` asked anonymously is a pass, and a filter that hid the
 * passes would hide it.
 */
export function failed(exchange: Exchange, expectUnauthorized: boolean): boolean {
  switch (exchange.kind) {
    case 'model': {
      const { http, stream } = exchange.outcome.response
      if (statusTone(http.status, expectUnauthorized) === 'bad') {
        return true
      }
      return stream !== undefined && !stream.terminated
    }
    case 'protocol': {
      const mcp = exchange.exchange
      return mcp.error !== undefined || mcp.status === 0 || mcp.status >= 400
    }
    case 'tool': {
      const tool = exchange.invocation
      return tool.error !== undefined || tool.reportedError || tool.schemaErrors.length > 0
    }
  }
}

/** Every way the loop can end, in a sentence. */
export function describeStop(stop: StopOutcome): { tone: Tone; text: string } {
  switch (stop.outcome) {
    case 'stopped':
      return stop.reason.predicate === 'noToolCalls'
        ? { tone: 'good', text: 'Stopped: the model asked for no more tools.' }
        : { tone: 'good', text: `Stopped: finish reason "${stop.reason.value}".` }
    case 'maxIterations':
      return { tone: 'warn', text: `Ran out of turns after ${stop.limit}.` }
    case 'deadline':
      return { tone: 'warn', text: `Ran out of time after ${stop.afterMs} ms.` }
    case 'repeatedCall':
      return {
        tone: 'bad',
        text: `The model asked for "${stop.tool}" again with the same arguments on turn ${stop.atTurn} — a loop, not progress.`,
      }
    case 'predicateNeverEvaluable':
      return {
        tone: 'bad',
        text: `\`${stop.predicate}\` could never be evaluated in ${stop.turns} turns — the endpoint never reported one. The loop was not slow, it was unfalsifiable.`,
      }
  }
}
