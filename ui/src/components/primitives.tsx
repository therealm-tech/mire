import { type ReactNode, useState } from 'react'

export type Tone = 'good' | 'bad' | 'warn' | 'neutral'

const TONE_CLASSES: Record<Tone, string> = {
  good: 'bg-good-soft text-good',
  bad: 'bg-bad-soft text-bad',
  warn: 'bg-warn-soft text-warn',
  neutral: 'bg-flag-soft text-muted',
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

/**
 * The one button shape, in the two weights the app actually uses.
 *
 * `primary` is the mark's centre block: solid ink on the sheet, inverted in the
 * dark. Everything else is an outline, so that a panel never has two things
 * competing to be the thing you press.
 */
export function Button({
  variant = 'ghost',
  size = 'sm',
  className = '',
  ...rest
}: {
  variant?: 'primary' | 'ghost'
  size?: 'sm' | 'md'
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const shape = size === 'md' ? 'rounded-lg px-4 py-1.5 text-sm' : 'rounded px-2 py-1 text-xs'
  const weight =
    variant === 'primary'
      ? 'bg-brand font-medium text-on-brand enabled:hover:opacity-90 selection:bg-on-brand selection:text-brand'
      : 'border border-line-strong enabled:hover:bg-well'

  return (
    <button
      type="button"
      className={`${shape} ${weight} transition-colors disabled:opacity-50 ${className}`}
      {...rest}
    />
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
    <section className="rounded-lg border border-line bg-panel">
      <header className="flex items-center justify-between gap-2 border-line border-b px-3 py-2">
        <h2 className="font-semibold text-sm">{title}</h2>
        {actions}
      </header>
      <div className="p-3">{children}</div>
    </section>
  )
}

export function Code({ children }: { children: string }) {
  return (
    <pre className="overflow-x-auto rounded bg-well p-2 font-mono text-xs leading-relaxed">
      {children}
    </pre>
  )
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false)

  return (
    <Button
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        })
      }}
    >
      {copied ? 'Copied' : label}
    </Button>
  )
}

/** A labelled row that collapses to one column on a phone. */
export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: the control is the `children` prop, so the association is implicit and correct.
    <label className="block space-y-1">
      <span className="font-medium text-muted text-xs">{label}</span>
      {children}
    </label>
  )
}

/**
 * The one input shape: a sheet you write on, ruled rather than boxed.
 *
 * Appearance only, with no width in it — a caller adding `w-16` to a `w-full`
 * would be two utilities in the same class list, and which of the two wins is a
 * question about stylesheet order rather than about the box.
 */
export const INPUT_CLASSES =
  'rounded border border-line-strong bg-panel px-2 py-1 text-ink text-sm placeholder:text-faint'

export function Spinner({ label }: { label: string }) {
  return (
    <span className="text-faint text-xs" role="status">
      {label}
    </span>
  )
}
