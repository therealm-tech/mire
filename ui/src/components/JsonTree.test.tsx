import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { JsonTree } from './JsonTree'

describe('JsonTree', () => {
  it('folds a branch away and back with its contents intact', async () => {
    const user = userEvent.setup()
    render(<JsonTree value={{ weather: { city: 'Paris', temp: 21 } }} />)

    expect(screen.getByText('city')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /weather · 2/ }))
    expect(screen.queryByText('city')).not.toBeInTheDocument()

    // Reopening has to bring it back, which it did not: the branch was handed an
    // *iterator* of its entries, the first render consumed it, and the render
    // the click caused found it empty. Collapsing anything therefore destroyed
    // it — and the body it was showing looked like it had never arrived.
    await user.click(screen.getByRole('button', { name: /weather · 2/ }))
    expect(screen.getByText('city')).toBeInTheDocument()
    expect(screen.getByText('"Paris"')).toBeInTheDocument()
    expect(screen.getByText('21')).toBeInTheDocument()
  })

  it('survives the same treatment on an array', async () => {
    const user = userEvent.setup()
    render(<JsonTree value={{ items: ['first', 'second'] }} />)

    const toggle = () => screen.getByRole('button', { name: /items · 2/ })
    await user.click(toggle())
    await user.click(toggle())

    expect(screen.getByText('"first"')).toBeInTheDocument()
    expect(screen.getByText('"second"')).toBeInTheDocument()
  })

  it('names a branch by its key rather than by its type', () => {
    render(<JsonTree value={{ params: { city: 'Paris' } }} />)

    // `object · 1` is the answer to a question nobody asked: the reader came
    // looking for a field, and the field name is what they have to go on.
    expect(screen.getByRole('button', { name: /params · 1/ })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /object · 1/ })).not.toBeInTheDocument()
  })

  it('opens the top of the tree and leaves what is deeper folded', () => {
    render(<JsonTree value={{ a: { b: { c: 1 } } }} />)

    // Two levels, which is what you want when hunting for where an endpoint hid
    // its content field: enough to see the shape, not enough to drown in it.
    expect(screen.getByText('a')).toBeInTheDocument()
    expect(screen.getByText('b')).toBeInTheDocument()
    expect(screen.queryByText('c')).not.toBeInTheDocument()
  })

  it('shows what is inside a branch it is not showing', async () => {
    const user = userEvent.setup()
    render(<JsonTree value={{ message: { role: 'user', content: 'ping' } }} />)

    await user.click(screen.getByRole('button', { name: /message · 2/ }))

    // The point of folding the deep half is that you can still tell which branch
    // you want; a row that only says `· 2` makes you open all of them to find out.
    expect(screen.getByText(/{ role, content }/)).toBeInTheDocument()
  })

  it('says an empty value in place rather than behind a toggle', () => {
    render(<JsonTree value={{ messages: [], meta: {} }} />)

    // Folding an empty array hides nothing, so there is nothing to fold: `[]` is
    // shorter than the toggle that would have revealed it.
    expect(screen.getByText('[]')).toBeInTheDocument()
    expect(screen.getByText('{}')).toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })
})
