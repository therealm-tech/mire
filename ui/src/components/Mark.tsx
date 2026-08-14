/**
 * The mark, as geometry rather than as a picture.
 *
 * Drawn rather than shipped as a raster: it is four shapes and a crosshair, so
 * an inline SVG is smaller than any PNG of it, stays sharp at any zoom, and —
 * the reason it is worth writing out — inherits `currentColor`, which is what
 * lets the same tag be ink on the sheet and cream in the dark without a second
 * asset to keep in step.
 *
 * A test pattern read from the outside in: the frame, the rings, four signals
 * converging, and what they converge on.
 */
export function Mark({ className = 'h-7 w-7' }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 64 64"
      className={className}
      fill="none"
      stroke="currentColor"
      role="img"
      aria-label="mire"
    >
      <rect x="4.5" y="4.5" width="55" height="55" strokeWidth="3" />
      <circle cx="32" cy="32" r="22" strokeWidth="3" />
      <circle cx="32" cy="32" r="15" strokeWidth="2.5" />

      {/* The crosshair, showing only where the rings leave it room. */}
      <g strokeWidth="3">
        <path d="M4 32h6M54 32h6M32 4v6M32 54v6" />
      </g>

      {/* Four signals, pointing at the one thing being measured. */}
      <g fill="currentColor" stroke="none">
        <path d="M32 24l-2.8-7h5.6zM32 40l-2.8 7h5.6zM24 32l-7-2.8v5.6zM40 32l7-2.8v5.6z" />
        <rect x="27.5" y="27.5" width="9" height="9" />
      </g>
    </svg>
  )
}
