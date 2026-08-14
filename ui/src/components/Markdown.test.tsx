import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { Markdown } from './Markdown'

describe('Markdown', () => {
  it('renders the marks instead of showing them', () => {
    const { container } = render(<Markdown>{'A **bold** word and `code`.'}</Markdown>)

    expect(screen.getByText('bold').tagName).toBe('STRONG')
    expect(screen.getByText('code').tagName).toBe('CODE')
    expect(container.textContent).not.toContain('**')
  })

  it('demotes headings so they fit inside a bubble', () => {
    render(<Markdown>{'# Title\n\n## Sub'}</Markdown>)

    expect(screen.getByText('Title').tagName).toBe('H3')
    expect(screen.getByText('Sub').tagName).toBe('H4')
  })

  it('renders lists as lists', () => {
    render(<Markdown>{'- one\n- two\n'}</Markdown>)

    expect(screen.getByRole('list').tagName).toBe('UL')
    expect(screen.getAllByRole('listitem')).toHaveLength(2)
  })

  it('scrolls a fenced block rather than styling it as an inline span', () => {
    const { container } = render(<Markdown>{'```rust\nfn main() {}\n```'}</Markdown>)

    const pre = container.querySelector('pre')
    expect(pre?.className).toContain('overflow-x-auto')
    // The `<code>` inside the fence must not carry the inline chrome, or every
    // block would be a rounded pill inside a rounded box.
    expect(container.querySelector('pre code')?.className).not.toContain('bg-well')
    expect(pre?.textContent).toContain('fn main() {}')
  })

  it('keeps the inline chrome on a span that is not in a fence', () => {
    const { container } = render(<Markdown>{'call `main()` first'}</Markdown>)

    expect(container.querySelector('code')?.className).toContain('bg-well')
    expect(container.querySelector('pre')).toBeNull()
  })

  it('renders a gfm table, which plain commonmark would not', () => {
    render(<Markdown>{'| a | b |\n| - | - |\n| 1 | 2 |'}</Markdown>)

    expect(screen.getByRole('table')).toBeInTheDocument()
    expect(screen.getByRole('columnheader', { name: 'a' })).toBeInTheDocument()
    expect(screen.getByRole('cell', { name: '2' })).toBeInTheDocument()
  })

  it('opens a link away from the app', () => {
    render(<Markdown>{'[docs](https://example.test/docs)'}</Markdown>)

    const link = screen.getByRole('link', { name: 'docs' })
    expect(link).toHaveAttribute('href', 'https://example.test/docs')
    expect(link).toHaveAttribute('target', '_blank')
    expect(link).toHaveAttribute('rel', expect.stringContaining('noreferrer'))
  })

  it('empties a javascript: href, because the model wrote it', () => {
    const { container } = render(<Markdown>{'[tap](javascript:alert(1))'}</Markdown>)

    // Emptied rather than dropped, so the text still reads — and an anchor with
    // no href is not even a link any more, which is the point.
    expect(container.querySelector('a')).toHaveAttribute('href', '')
    expect(screen.queryByRole('link')).toBeNull()
    expect(screen.getByText('tap')).toBeInTheDocument()
  })

  it('treats raw html as text, not as markup', () => {
    const { container } = render(
      <Markdown>{'<img src=x onerror="alert(1)"> and <b>not bold</b>'}</Markdown>,
    )

    expect(container.querySelector('img')).toBeNull()
    expect(container.querySelector('b')).toBeNull()
    expect(container.textContent).toContain('<b>not bold</b>')
  })

  it('renders a half-arrived fence rather than waiting for its close', () => {
    const { container } = render(<Markdown>{'```\nfn main() {'}</Markdown>)

    expect(container.querySelector('pre')?.textContent).toContain('fn main() {')
  })
})
