import { useEffect, useState } from 'react'

/**
 * Whether a media query holds, and whether it still does.
 *
 * Used for one thing: the profile list is a column on a laptop and a fold-away
 * on a phone, and which of those it should be is not a thing CSS can decide for
 * a React tree that renders two different sets of controls.
 *
 * A missing `matchMedia` answers `false` rather than throwing. That is a browser
 * old enough that nobody is running this in it, or a test environment — and
 * neither is worth a blank page.
 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return false
    }
    return window.matchMedia(query).matches
  })

  useEffect(() => {
    if (typeof window.matchMedia !== 'function') {
      return
    }
    const list = window.matchMedia(query)
    const update = (event: MediaQueryListEvent) => setMatches(event.matches)
    // Read once on the way in as well: the query can have changed between the
    // first render and this effect, and a rotated phone would be showing the
    // wrong controls until it was rotated again.
    setMatches(list.matches)
    list.addEventListener('change', update)
    return () => list.removeEventListener('change', update)
  }, [query])

  return matches
}
