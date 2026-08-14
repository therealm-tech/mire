import type { Message } from './api'
import type { Exchange } from './conversation'

/**
 * A run, as something you can hand to somebody else.
 *
 * The panels are for reading; this is for sending. "It works here and not
 * there" is settled by the bytes, and until now the only way out of this page
 * was *Copy as curl* one request at a time — which reproduces a call but loses
 * the run: the order, the turns, what the decoder made of each answer.
 *
 * It is exactly what was on screen, in the shape the API sent it. Nothing is
 * summarised on the way out, because the person you are sending it to is going
 * to want the part you did not think was interesting.
 */
export interface RunExport {
  tool: 'mire'
  exportedAt: string
  profile: string | null
  /** Where it was pointed, and who it went as. */
  endpoint: string | null
  identity: string | null
  /** The history as the next request would have carried it. */
  messages: Message[]
  exchanges: Exchange[]
}

export function runExport({
  profile,
  endpoint,
  identity,
  messages,
  exchanges,
  at,
}: {
  profile: string | null
  endpoint: string | null
  identity: string | null
  messages: Message[]
  exchanges: Exchange[]
  /** Passed in rather than read here, so the shape stays a pure function. */
  at: Date
}): RunExport {
  return {
    tool: 'mire',
    exportedAt: at.toISOString(),
    profile,
    endpoint,
    identity,
    messages,
    exchanges,
  }
}

/** `mire-chat-2026-08-14T09-31-07.json` — sortable, and safe on every filesystem. */
export function exportFilename(profile: string | null, at: Date): string {
  const stamp = at
    .toISOString()
    .replace(/\.\d+Z$/, '')
    .replace(/:/g, '-')
  return `mire-${profile ?? 'run'}-${stamp}.json`
}

/**
 * Hands the browser a file.
 *
 * An object URL and a synthetic click, which is the only way to do this without
 * a server to fetch it from — and there is no server here to ask: the whole
 * point is that this never left the tab.
 */
export function download(filename: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }))
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  anchor.click()
  URL.revokeObjectURL(url)
}
