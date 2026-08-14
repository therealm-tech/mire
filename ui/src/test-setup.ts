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
 * the profile column. The narrow case overrides this in its own test.
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
