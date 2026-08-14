import { type Dispatch, type SetStateAction, useEffect, useState } from 'react'
import type { z } from 'zod'

/**
 * What this tab remembers across a reload.
 *
 * Settings and a draft — which profile you were on, what you had half typed, how
 * many turns you allow. Small, bounded things whose loss is pure annoyance.
 *
 * Three things are deliberately not here. **The credential**, because a token
 * typed into this tab lives in this tab and nowhere else, and putting it on disk
 * would quietly turn that promise into a lie. **The conversation and the
 * traffic**, because a session's bodies are unbounded and the first oversized
 * run would start throwing `QuotaExceededError` at a tool whose job is to be
 * dependable while other things fail. And **anything the server owns**, which is
 * reloaded from it on every start and cannot go stale here if it is never kept.
 *
 * `mire` still holds nothing: this is the browser's own storage, on the same
 * side of the wire as the conversation has always been.
 */
const PREFIX = 'mire.'

/**
 * Whatever was left here last time, if it is still the shape we expect.
 *
 * Anything unreadable is dropped rather than repaired: a stored value from an
 * older build is not worth a migration, and storage itself can be missing
 * entirely — disabled cookies, a hardened profile — which is a reason to have no
 * memory, never a reason to fail to start.
 */
function load<T>(key: string, schema: z.ZodType<T>): T | undefined {
  try {
    const raw = window.localStorage.getItem(PREFIX + key)
    if (raw === null) {
      return undefined
    }
    const parsed = schema.safeParse(JSON.parse(raw))
    return parsed.success ? parsed.data : undefined
  } catch {
    return undefined
  }
}

function save(key: string, value: unknown): void {
  try {
    window.localStorage.setItem(PREFIX + key, JSON.stringify(value))
  } catch {
    // Full, or not there at all. Neither is worth interrupting anybody over.
  }
}

/** `useState`, with what it held last time as its starting point. */
export function usePersisted<T>(
  key: string,
  schema: z.ZodType<T>,
  initial: T,
): [T, Dispatch<SetStateAction<T>>] {
  const [value, setValue] = useState<T>(() => load(key, schema) ?? initial)

  useEffect(() => {
    save(key, value)
  }, [key, value])

  return [value, setValue]
}
