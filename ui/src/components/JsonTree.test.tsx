import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { JsonTree } from './JsonTree'

describe('JsonTree', () => {
  it('folds a branch away and back with its contents intact', async () => {
    const user = userEvent.setup()
    render(<JsonTree value={{ city: 'Paris', temp: 21 }} />)

    expect(screen.getByText('city')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /object · 2/ }))
    expect(screen.queryByText('city')).not.toBeInTheDocument()

    // Reopening has to bring it back, which it did not: the branch was handed an
    // *iterator* of its entries, the first render consumed it, and the render
    // the click caused found it empty. Collapsing anything therefore destroyed
    // it — and the body it was showing looked like it had never arrived.
    await user.click(screen.getByRole('button', { name: /object · 2/ }))
    expect(screen.getByText('city')).toBeInTheDocument()
    expect(screen.getByText('"Paris"')).toBeInTheDocument()
    expect(screen.getByText('21')).toBeInTheDocument()
  })

  it('survives the same treatment on an array', async () => {
    const user = userEvent.setup()
    render(<JsonTree value={['first', 'second']} />)

    const toggle = () => screen.getByRole('button', { name: /array · 2/ })
    await user.click(toggle())
    await user.click(toggle())

    expect(screen.getByText('"first"')).toBeInTheDocument()
    expect(screen.getByText('"second"')).toBeInTheDocument()
  })

  it('opens the top of the tree and leaves what is deeper folded', () => {
    render(<JsonTree value={{ a: { b: { c: 1 } } }} />)

    // Two levels, which is what you want when hunting for where an endpoint hid
    // its content field: enough to see the shape, not enough to drown in it.
    expect(screen.getByText('a')).toBeInTheDocument()
    expect(screen.getByText('b')).toBeInTheDocument()
    expect(screen.queryByText('c')).not.toBeInTheDocument()
  })
})
