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
    return <span className="text-string">"{value}"</span>
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
