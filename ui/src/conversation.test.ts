import { describe, expect, it } from 'vitest'
import type { HookRecord, ToolInvocation } from './api'
import { activityItem, type Exchange, type HookExchange, type ToolExchange } from './conversation'

/** Ids only have to be distinct here; the module makes its own for real runs. */
let counter = 0
function id(prefix: string): string {
  counter += 1
  return `${prefix}-${counter}`
}

/**
 * Which hook belongs to which call, which is the one thing a summary row cannot
 * get wrong quietly.
 *
 * A row filed under the wrong call is not a cosmetic slip: the reading it is
 * there to support — the gate, the call it let through, the audit that followed
 * — becomes a sentence about a call that never happened.
 */

function tool(name: string): ToolExchange {
  const invocation: ToolInvocation = {
    call: { name, arguments: {} },
    source: 'mcp',
    server: 'weather',
    reportedError: false,
    schemaErrors: [],
    result: 'ok',
    captured: {},
  }
  return { kind: 'tool', id: id('tool'), turn: 1, invocation }
}

function hook(name: string, phase: 'before' | 'after', on: string): HookExchange {
  const record: HookRecord = {
    server: 'weather',
    hook: name,
    step: 1,
    phase,
    tool: on,
    action: 'http',
    url: `https://audit.internal/${name}`,
    method: 'POST',
    headers: {},
    request: '',
    files: [],
    status: 204,
    response: '',
    latencyMs: 3,
    stoppedTheCall: false,
  }
  return { kind: 'hook', id: id('hook'), turn: 1, record }
}

/** The steps of the row, as `hook:name` and `tool:name`, in order. */
function steps(exchanges: Exchange[]): string[] {
  return activityItem(1, exchanges).steps.map((step) =>
    step.kind === 'tool' ? `tool:${step.invocation.call.name}` : `hook:${step.record.hook}`,
  )
}

describe('hooks around the call they fired on', () => {
  it('puts the gate above the call and the audit below it', () => {
    const exchanges = [
      hook('gate', 'before', 'get_weather'),
      hook('audit', 'after', 'get_weather'),
      tool('get_weather'),
    ]

    expect(steps(exchanges)).toEqual(['hook:gate', 'tool:get_weather', 'hook:audit'])
  })

  it('leaves each call its own hook when only the after side fires', () => {
    // The quiet one: two `after` records in a row used to be read as two hooks
    // of the *first* call, which filed the second call's audit under a call it
    // had nothing to do with.
    const exchanges = [
      hook('audit', 'after', 'get_weather'),
      hook('audit', 'after', 'get_forecast'),
      tool('get_weather'),
      tool('get_forecast'),
    ]

    expect(steps(exchanges)).toEqual([
      'tool:get_weather',
      'hook:audit',
      'tool:get_forecast',
      'hook:audit',
    ])
  })

  it('splits the hooks of one tool called twice in the same turn', () => {
    // Here the tool name settles nothing — both records name `get_weather`. What
    // does is that a hook fires once per call, so its name coming round again is
    // the next call's traffic.
    const exchanges = [
      hook('audit', 'after', 'get_weather'),
      hook('audit', 'after', 'get_weather'),
      tool('get_weather'),
      tool('get_weather'),
    ]

    expect(steps(exchanges)).toEqual([
      'tool:get_weather',
      'hook:audit',
      'tool:get_weather',
      'hook:audit',
    ])
  })

  it('keeps every action of one hook with the call it fired on', () => {
    const first = hook('upload', 'before', 'run_task')
    const second = hook('upload', 'before', 'run_task')
    second.record = { ...second.record, step: 2 }

    expect(steps([first, second, tool('run_task')])).toEqual([
      'hook:upload',
      'hook:upload',
      'tool:run_task',
    ])
  })

  it('shows a hook whose call the turn does not carry rather than dropping it', () => {
    const exchanges = [hook('audit', 'after', 'a_tool_nobody_called'), tool('get_weather')]

    expect(steps(exchanges)).toEqual(['tool:get_weather', 'hook:audit'])
  })
})
