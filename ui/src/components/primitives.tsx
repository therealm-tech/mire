import { type ReactNode, useState } from 'react'

export type Tone = 'good' | 'bad' | 'warn' | 'neutral'

const TONE_CLASSES: Record<Tone, string> = {
  good: 'bg-emerald-100 text-emerald-900 dark:bg-emerald-950 dark:text-emerald-200',
  bad: 'bg-rose-100 text-rose-900 dark:bg-rose-950 dark:text-rose-200',
  warn: 'bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-200',
  neutral: 'bg-stone-200 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
}

export function Badge({ tone = 'neutral', children }: { tone?: Tone; children: ReactNode }) {
  return (
    <span
      className={`inline-flex items-center rounded px-2 py-0.5 font-medium text-xs ${TONE_CLASSES[tone]}`}
    >
      {children}
    </span>
  )
}

export function Panel({
  title,
  actions,
  children,
}: {
  title: string
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="rounded-lg border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900">
      <header className="flex items-center justify-between gap-2 border-stone-200 border-b px-3 py-2 dark:border-stone-800">
        <h2 className="font-semibold text-sm">{title}</h2>
        {actions}
      </header>
      <div className="p-3">{children}</div>
    </section>
  )
}

export function Code({ children }: { children: string }) {
  return (
    <pre className="overflow-x-auto rounded bg-stone-100 p-2 font-mono text-xs leading-relaxed dark:bg-stone-950">
      {children}
    </pre>
  )
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false)

  return (
    <button
      type="button"
      className="rounded border border-stone-300 px-2 py-1 text-xs hover:bg-stone-100 dark:border-stone-700 dark:hover:bg-stone-800"
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        })
      }}
    >
      {copied ? 'Copied' : label}
    </button>
  )
}

/** A labelled row that collapses to one column on a phone. */
export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: the control is the `children` prop, so the association is implicit and correct.
    <label className="block space-y-1">
      <span className="font-medium text-stone-600 text-xs dark:text-stone-400">{label}</span>
      {children}
    </label>
  )
}

export function Spinner({ label }: { label: string }) {
  return (
    <span className="text-stone-500 text-xs dark:text-stone-400" role="status">
      {label}
    </span>
  )
}
