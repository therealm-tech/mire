import { useState } from 'react'

/**
 * A collapsible view of arbitrary JSON.
 *
 * Objects and arrays start open at the top and closed below, which is what you
 * want when you are trying to find where an endpoint hid its content field.
 */
export function JsonTree({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value === null) {
    return <span className="text-faint">null</span>
  }
  if (typeof value === 'string') {
    return <Text value={value} />
  }
  if (typeof value === 'number' || typeof value === 'boolean') {
    return <span className="text-number">{String(value)}</span>
  }
  if (Array.isArray(value)) {
    return <Branch label={`array · ${value.length}`} entries={[...value.entries()]} depth={depth} />
  }
  if (typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>)
    return <Branch label={`object · ${entries.length}`} entries={entries} depth={depth} />
  }
  return <span className="text-faint">{String(value)}</span>
}

/**
 * How much of a string is shown before it has to ask.
 *
 * Long enough that no field an endpoint returns is ever elided; short enough
 * that the megabyte of base64 an attached file becomes does not have to be
 * painted to find the field next to it.
 */
const ELIDE_OVER = 400
const KEEP = 200

/**
 * A string, folded when it is really a file.
 *
 * Same bargain as the tree around it: nothing is hidden, but nothing is forced
 * on you either. It is still on the wire in full — *Copy as curl* and *Copy
 * body* both hand it over whole, because that is what reproduces the call.
 */
function Text({ value }: { value: string }) {
  const [open, setOpen] = useState(false)

  if (value.length <= ELIDE_OVER || open) {
    return <span className="text-string">"{value}"</span>
  }
  return (
    <span className="text-string">
      "{value.slice(0, KEEP)}
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="mx-1 rounded bg-well px-1 text-muted hover:text-ink hover:underline"
      >
        … {value.length - KEEP} more characters
      </button>
      "
    </span>
  )
}

function Branch({
  label,
  entries,
  depth,
}: {
  label: string
  /**
   * An array, and it has to be: an iterator is consumed by the first render, and
   * the second one — the one the collapse toggle causes — would find it empty.
   * The parent does not re-render, so it is the *same* exhausted iterator.
   */
  entries: [number | string, unknown][]
  depth: number
}) {
  const [open, setOpen] = useState(depth < 2)

  return (
    <div>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="text-faint text-xs hover:text-ink hover:underline"
      >
        {open ? '▾' : '▸'} {label}
      </button>
      {open ? (
        <ul className="ml-3 border-line border-l pl-2">
          {entries.map(([key, nested]) => (
            <li key={String(key)} className="py-px">
              <span className="text-muted">{key}</span>
              <span className="text-faint">: </span>
              <JsonTree value={nested} depth={depth + 1} />
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}
