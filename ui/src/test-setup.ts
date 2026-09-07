import { beforeEach } from 'vitest'
import '@testing-library/jest-dom/vitest'

// jsdom has no layout, so it ships no `scrollIntoView`. The transcript keeps its
// end in sight with one, and a missing method is a `TypeError`, not a no-op.
Element.prototype.scrollIntoView = () => {}

/**
 * Somewhere for the UI to remember its settings.
 *
 * Not jsdom's: recent Node injects a `localStorage` global of its own, which
 * shadows it and answers a `TypeError` to every method. The app survives that —
 * every access is guarded, and a browser that cannot remember is a browser with
 * no memory rather than a broken page — but a test suite that inherited it would
 * be exercising the guard on every run and the feature on none of them.
 */
class MemoryStorage implements Storage {
  private entries = new Map<string, string>()

  get length(): number {
    return this.entries.size
  }
  key(index: number): string | null {
    return [...this.entries.keys()][index] ?? null
  }
  getItem(key: string): string | null {
    return this.entries.get(key) ?? null
  }
  setItem(key: string, value: string): void {
    this.entries.set(key, String(value))
  }
  removeItem(key: string): void {
    this.entries.delete(key)
  }
  clear(): void {
    this.entries.clear()
  }
}

Object.defineProperty(window, 'localStorage', {
  configurable: true,
  writable: true,
  value: new MemoryStorage(),
})

/**
 * A laptop, which is what a tool you run next to your work is looked at on.
 *
 * jsdom ships no `matchMedia`, and the layout asks it whether there is room for
 * the model column. The narrow case overrides this in its own test.
 */
Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  writable: true,
  value: (query: string) => ({
    matches: true,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }),
})

// A setting one test changes is not a setting the next one inherits.
beforeEach(() => {
  window.localStorage.clear()
})

/**
 * The configuration stream, which jsdom does not implement.
 *
 * `EventSource` is missing from jsdom entirely, and the app opens one on mount —
 * so without this every test renders against a `ReferenceError`. It is a stub
 * with a handle rather than a silent no-op, because "the page follows a reload"
 * is behaviour worth a test, and the only way to have one is to be able to
 * announce a reload.
 */
class FakeEventSource {
  readonly url: string
  closed = false
  private readonly listeners = new Map<string, Set<(event: Event) => void>>()

  constructor(url: string) {
    this.url = url
    streams.push(this)
  }

  addEventListener(type: string, listener: (event: Event) => void): void {
    const existing = this.listeners.get(type) ?? new Set()
    existing.add(listener)
    this.listeners.set(type, existing)
  }

  removeEventListener(type: string, listener: (event: Event) => void): void {
    this.listeners.get(type)?.delete(listener)
  }

  close(): void {
    this.closed = true
  }

  /** A reload landing, the way `mire` announces one. */
  announce(generation: number): void {
    this.emit(new MessageEvent('config', { data: JSON.stringify({ event: 'config', generation }) }))
  }

  /**
   * The stream coming up.
   *
   * Called by hand rather than fired from the constructor, because the listeners
   * are attached after it returns — and because a *re*connection is this a
   * second time, which is the case worth testing: the tab was away, and cannot
   * know what it missed.
   */
  connect(): void {
    this.emit(new Event('open'))
  }

  private emit(event: Event): void {
    for (const listener of this.listeners.get(event.type) ?? []) {
      listener(event)
    }
  }
}

const streams: FakeEventSource[] = []

/** The stream the page currently has open. */
export function configStream(): FakeEventSource {
  const open = streams.filter((stream) => !stream.closed)
  const latest = open[open.length - 1]
  if (!latest) {
    throw new Error('the page has no configuration stream open')
  }
  return latest
}

Object.defineProperty(window, 'EventSource', {
  configurable: true,
  writable: true,
  value: FakeEventSource,
})

// A stream one test opened is not a stream the next one inherits.
beforeEach(() => {
  streams.length = 0
})
