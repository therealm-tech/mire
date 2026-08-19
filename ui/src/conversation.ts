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
  HookRecord,
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
  steps: Array<ToolExchange | HookExchange>
}

/** How a run ended, parked where it happened in the transcript. */
export interface VerdictItem {
  kind: 'verdict'
  id: string
  stop: StopOutcome
  turns: number
  durationMs: number
  /**
   * The call the run ended on, because how the loop stopped is only half of how
   * it went: a `400` decodes to no tool calls, which the loop reads as a model
   * with nothing left to ask for. Null when the run never made a call.
   */
  call: CallOutcome | null
}

/**
 * The answer as it is being read, and what the endpoint said about it.
 *
 * `status` stays null until the stream ends — and after it, a null means the
 * stream broke before the `done` event ever landed.
 */
export interface Live {
  text: string
  status: number | null
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
    steps: around(
      exchanges.filter((exchange) => exchange.kind === 'tool'),
      exchanges.filter((exchange) => exchange.kind === 'hook'),
    ),
  }
}

/**
 * Tool calls with the hooks that fired around them, in the order it happened.
 *
 * The trace keeps the two apart — a hook is traffic to a third address, not a
 * tool call — and the reading a person wants puts them back together: the gate,
 * the call it let through, the audit that followed. A turn fires every `before`
 * hook, makes the call, then fires every `after` hook, and only then moves on,
 * so the hooks are consumed from the front in that order.
 *
 * The phase alone is not enough to say where one call's hooks end, and the
 * failure is quiet: two calls with an `after`-only hook each produce
 * `[after, after]`, and taking every consecutive `after` files the second call's
 * hook under the first. So a hook is only taken for the call it *names*, and a
 * hook whose name has already been taken for the call in hand is the next call's
 * traffic — which is what settles the case the tool name cannot, the same tool
 * called twice in one turn.
 *
 * A hook left over at the end fired around a call the turn does not carry, which
 * should not happen — and if it ever does, it is shown rather than dropped.
 */
function around(tools: ToolExchange[], hooks: HookExchange[]): Array<ToolExchange | HookExchange> {
  const steps: Array<ToolExchange | HookExchange> = []
  let next = 0
  const take = (phase: 'before' | 'after', tool: string) => {
    const taken = new Set<string>()
    for (let hook = hooks[next]; hook !== undefined; hook = hooks[next]) {
      const record = hook.record
      if (record.phase !== phase || record.tool !== tool) {
        return
      }
      // One firing per action per call, so a name coming round again is the
      // next call's.
      const name = `${record.server}·${record.hook}·${record.step}`
      if (taken.has(name)) {
        return
      }
      taken.add(name)
      steps.push(hook)
      next += 1
    }
  }

  for (const tool of tools) {
    take('before', tool.invocation.call.name)
    steps.push(tool)
    take('after', tool.invocation.call.name)
  }
  return [...steps, ...hooks.slice(next)]
}

export function verdictItem(trace: Trace): VerdictItem {
  return {
    kind: 'verdict',
    id: nextId('verdict'),
    stop: trace.stop,
    turns: trace.turns.length,
    durationMs: trace.durationMs,
    call: trace.turns.at(-1)?.call ?? null,
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

/**
 * One hook firing around a tool call.
 *
 * Its own card rather than a line on the tool's, because it is traffic to a
 * third address: when a gate refuses a call, the tool card says the call did not
 * happen and only this says who decided that, and what they answered.
 */
export interface HookExchange {
  kind: 'hook'
  id: string
  turn: number | null
  record: HookRecord
}

export type Exchange = ModelExchange | ToolExchange | ProtocolExchange | HookExchange

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
  const hooks: HookExchange[] = turn.hooks.map((record) => ({
    kind: 'hook',
    id: nextId('hook'),
    turn: turn.index,
    record,
  }))
  const tools: ToolExchange[] = turn.tools.map((invocation) => ({
    kind: 'tool',
    id: nextId('tool'),
    turn: turn.index,
    invocation,
  }))
  return [model, ...protocol, ...hooks, ...tools]
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
 * The failures, in the six shapes they come in: a status the endpoint should
 * not have answered, an error the endpoint reported in the body whatever status
 * it put on top, a stream that stopped rather than ended, a protocol round trip
 * that never landed, a tool that failed, reported a problem, or was called with
 * arguments its own schema refuses, and a hook that was refused or never landed.
 * `expectUnauthorized` is carried through because a `401` asked anonymously is a
 * pass, and a filter that hid the passes would hide it.
 */
export function failed(exchange: Exchange, expectUnauthorized: boolean): boolean {
  switch (exchange.kind) {
    case 'model': {
      const { http, error, stream } = exchange.outcome.response
      if (statusTone(http.status, expectUnauthorized) === 'bad') {
        return true
      }
      // A gateway that swallows the upstream failure and answers `200` has
      // still failed, and this filter is where you go looking for it.
      if (error !== undefined) {
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
    case 'hook': {
      const hook = exchange.record
      // A hook that sat a call out did not fail: `if:` said no, nothing was
      // sent, and the tool call went ahead untouched. Its `status: 0` means
      // "never sent", not "never answered", and a filter that pulled it in would
      // say a run went wrong every time a session is opened partway.
      if (hook.skipped !== undefined) {
        return false
      }
      return hook.error !== undefined || hook.status === 0 || hook.status >= 400
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

/**
 * How the run ended, with the endpoint's own answer in front of it.
 *
 * The loop's account of itself is not the whole verdict: a `400` carries no
 * tool calls, so the loop stops on "the model asked for no more tools" and the
 * run reads green while the endpoint was refusing it. So the status comes first
 * and settles the colour, and the loop still gets to say why it stopped.
 */
export function describeVerdict(
  item: VerdictItem,
  expectUnauthorized: boolean,
): { tone: Tone; text: string } {
  const stopped = describeStop(item.stop)
  const response = item.call?.response
  if (response === undefined) {
    return stopped
  }
  const status = response.http.status
  if (statusTone(status, expectUnauthorized) !== 'good') {
    return { tone: 'bad', text: `The endpoint answered ${status}. ${stopped.text}` }
  }
  // A gateway that answers `200` over an upstream failure has still failed, and
  // a verdict that called that one green would be the same lie a status short.
  if (response.error !== undefined) {
    return {
      tone: 'bad',
      text: `The endpoint answered ${status} with an error in the body. ${stopped.text}`,
    }
  }
  return stopped
}

/**
 * What the badge over a streaming answer says, while it arrives and once it has.
 *
 * `complete` is a claim about the endpoint, not about the request finishing, so
 * it is only ever said over a status that earned it: a stream that opened `400`
 * and wrote nothing is a run that failed, however tidily it ended. A run called
 * off from this tab is neither — nobody was refused, the question was withdrawn.
 */
export function describeLive(
  live: Live,
  run: { busy: boolean; stopped: boolean },
  expectUnauthorized: boolean,
): { tone: Tone; label: string } {
  if (run.busy) {
    return { tone: 'neutral', label: 'receiving…' }
  }
  if (run.stopped) {
    return { tone: 'warn', label: 'called off' }
  }
  if (live.status === null) {
    return { tone: 'bad', label: 'never answered' }
  }
  const tone = statusTone(live.status, expectUnauthorized)
  return tone === 'good' ? { tone, label: 'complete' } : { tone, label: `answered ${live.status}` }
}
