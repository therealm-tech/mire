/**
 * One structured logger for the whole app.
 *
 * Key-values, never interpolation, and the level comes from the environment so
 * that turning verbosity up does not mean editing call sites.
 */

const LEVELS = ['debug', 'info', 'warn', 'error'] as const

export type Level = (typeof LEVELS)[number]

type Fields = Record<string, unknown>

function configuredLevel(): Level {
  const raw = import.meta.env.VITE_LOG_LEVEL
  const found = LEVELS.find((level) => level === raw)
  return found ?? (import.meta.env.DEV ? 'debug' : 'info')
}

const threshold = LEVELS.indexOf(configuredLevel())

function emit(level: Level, event: string, fields?: Fields): void {
  if (LEVELS.indexOf(level) < threshold) {
    return
  }
  // biome-ignore lint/suspicious/noConsole: this module is the one place that owns the console.
  console[level === 'debug' ? 'log' : level](event, fields ?? {})
}

export const logger = {
  debug: (event: string, fields?: Fields) => emit('debug', event, fields),
  info: (event: string, fields?: Fields) => emit('info', event, fields),
  warn: (event: string, fields?: Fields) => emit('warn', event, fields),
  error: (event: string, fields?: Fields) => emit('error', event, fields),
}
