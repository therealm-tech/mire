import { useState } from 'react'

/**
 * A collapsible view of arbitrary JSON, labelled by the key rather than by the
 * type.
 *
 * `▸ object · 3` answers a question nobody asked. A reader hunting for where an
 * endpoint hid its content field came for a *name*, and the name was sitting on
 * the parent line all along — so the foldable node and the key line are the same
 * line here: the key, how much is under it, and a glimpse of what while it is
 * folded. The glimpse is what stops the hunt being a matter of opening every
 * branch to find out which one it was.
 *
 * The top level renders its fields directly rather than under a node of their
 * own: the caller already framed this as a body, and a `▾ object · 6` above it
 * is one click and one line spent restating that.
 */
export function JsonTree({ value, depth = 0 }: { value: unknown; depth?: number }) {
  const entries = children(value)
  return entries === null ? <Leaf value={value} /> : <Nodes entries={entries} depth={depth} />
}

/**
 * The children of a value, or `null` when it has none a reader would fold.
 *
 * An empty object is not a branch: folding it hides nothing, and `{}` said in
 * place is shorter than the toggle that would reveal it.
 */
function children(value: unknown): [string, unknown][] | null {
  if (Array.isArray(value)) {
    return value.length === 0 ? null : value.map((nested, index) => [String(index), nested])
  }
  if (typeof value === 'object' && value !== null) {
    const entries = Object.entries(value)
    return entries.length === 0 ? null : entries
  }
  return null
}

function Nodes({ entries, depth }: { entries: [string, unknown][]; depth: number }) {
  return (
    <ul className={depth === 0 ? '' : 'ml-3 border-line border-l pl-2'}>
      {entries.map(([key, nested]) => (
        <li key={key} className="py-px">
          <Node name={key} value={nested} depth={depth} />
        </li>
      ))}
    </ul>
  )
}

function Node({ name, value, depth }: { name: string; value: unknown; depth: number }) {
  const entries = children(value)
  if (entries === null) {
    return (
      <>
        <span className="text-muted">{name}</span>
        <span className="text-faint">: </span>
        <Leaf value={value} />
      </>
    )
  }
  return <Branch name={name} value={value} entries={entries} depth={depth} />
}

/**
 * One foldable field.
 *
 * Open at the top and folded below, which is the shape of the question: the
 * fields of the answer, and then only the one you asked for.
 */
function Branch({
  name,
  value,
  entries,
  depth,
}: {
  name: string
  value: unknown
  /**
   * An array, and it has to be: an iterator is consumed by the first render, and
   * the second one — the one the collapse toggle causes — would find it empty.
   * The parent does not re-render, so it is the *same* exhausted iterator.
   */
  entries: [string, unknown][]
  depth: number
}) {
  const [open, setOpen] = useState(depth < 1)

  return (
    <div>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        // Spelled out rather than left to the spans below: the name of a button
        // is computed by trimming each descendant's text and joining the pieces,
        // so the spacing between them is dropped and this one was read aloud as
        // `▾params· 1`.
        aria-label={`${name} · ${entries.length}`}
        className="max-w-full truncate text-left hover:underline"
      >
        <span className="text-faint">{open ? '▾' : '▸'}</span>
        <span className="text-muted">{` ${name}`}</span>
        <span className="text-faint">{` · ${entries.length}`}</span>
        {open ? null : <span className="text-faint">{` ${peek(value)}`}</span>}
      </button>
      {open ? <Nodes entries={entries} depth={depth + 1} /> : null}
    </div>
  )
}

function Leaf({ value }: { value: unknown }) {
  if (value === null) {
    return <span className="text-faint">null</span>
  }
  if (typeof value === 'string') {
    return <span className="text-string">"{value}"</span>
  }
  if (typeof value === 'number' || typeof value === 'boolean') {
    return <span className="text-number">{String(value)}</span>
  }
  if (Array.isArray(value)) {
    return <span className="text-faint">[]</span>
  }
  if (typeof value === 'object') {
    return <span className="text-faint">{'{}'}</span>
  }
  return <span className="text-faint">{String(value)}</span>
}

/** How long a folded glimpse is allowed to be before it stops being one. */
const PEEK_LIMIT = 48

/**
 * What is inside, for a branch that is closed.
 *
 * Enough to recognise the thing without opening it — the values of a short list,
 * the field names of an object, the shape of the items in a list of them — and
 * never more than a line, because a glimpse that wraps is not a glimpse.
 */
function peek(value: unknown): string {
  if (Array.isArray(value)) {
    const shape = children(value[0]) === null ? null : keys(value[0])
    return shape === null ? clip(`[${value.map(scalar).join(', ')}]`) : clip(`[${shape}, …]`)
  }
  return clip(keys(value))
}

/** An object's field names, in braces. */
function keys(value: unknown): string {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return scalar(value)
  }
  return `{ ${Object.keys(value).join(', ')} }`
}

function scalar(value: unknown): string {
  if (typeof value === 'string') {
    return `"${value}"`
  }
  if (Array.isArray(value)) {
    return '[…]'
  }
  if (typeof value === 'object' && value !== null) {
    return '{…}'
  }
  return String(value)
}

function clip(text: string): string {
  return text.length <= PEEK_LIMIT ? text : `${text.slice(0, PEEK_LIMIT - 1)}…`
}
