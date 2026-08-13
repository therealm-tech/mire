import '@testing-library/jest-dom/vitest'

// jsdom has no layout, so it ships no `scrollIntoView`. The transcript keeps its
// end in sight with one, and a missing method is a `TypeError`, not a no-op.
Element.prototype.scrollIntoView = () => {}
