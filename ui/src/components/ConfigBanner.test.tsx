import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import type { ConfigDirectory } from '../api'
import { ConfigBanner } from './ConfigBanner'

function directory(name: string, loaded: number, messages: string[] = []): ConfigDirectory {
  return {
    name,
    loaded,
    issues: messages.map((message, index) => ({
      file: `/tmp/config/${name}/broken-${index}.yaml`,
      message,
      line: null,
      column: null,
    })),
  }
}

/** Everything loaded, which is the state the page spends its life in. */
const CLEAN: ConfigDirectory[] = [
  directory('models', 2),
  directory('auth', 1),
  directory('mcp', 0),
  directory('prompts', 4),
  directory('decodes', 6),
]

describe('ConfigBanner', () => {
  it('is not there at all when every file loaded', () => {
    const { container } = render(<ConfigBanner directories={CLEAN} />)

    expect(container).toBeEmptyDOMElement()
  })

  it('counts every file across every directory, not the directories', () => {
    render(
      <ConfigBanner
        directories={[
          directory('models', 1, ['unknown field `templat`']),
          directory('auth', 1),
          directory('mcp', 0, ['duplicate stage `prod`']),
          directory('prompts', 4, ['duplicate prompt name `strict-json`']),
          directory('decodes', 6),
        ]}
      />,
    )

    expect(screen.getByText('3 files of the configuration did not load')).toBeInTheDocument()
    // Only the ones with something to say: a directory that loaded cleanly is
    // not a line saying it loaded cleanly.
    expect(screen.getByText('models/ · mcp/ · prompts/')).toBeInTheDocument()
  })

  it('counts one file as one file', () => {
    render(<ConfigBanner directories={[directory('models', 1, ['unknown field `templat`'])]} />)

    expect(screen.getByText('1 file of the configuration did not load')).toBeInTheDocument()
  })

  /**
   * The directory nothing else can speak for: `decodes/` has no listing of its
   * own, so before this banner its issues reached the log and stopped there.
   */
  it('shows what no listing carries', () => {
    render(
      <ConfigBanner
        directories={[directory('decodes', 6, ['invalid JSON pointer `/choices/0/mesage`'])]}
      />,
    )

    expect(screen.getByText('invalid JSON pointer `/choices/0/mesage`')).toBeInTheDocument()
    expect(screen.getByText('decodes/', { selector: 'h3' })).toBeInTheDocument()
  })

  it('names the file, and the position when the parser reports one', () => {
    render(
      <ConfigBanner
        directories={[
          {
            name: 'models',
            loaded: 1,
            issues: [
              { file: '/tmp/models/qwen3.yaml', message: 'unknown field', line: 14, column: 3 },
              {
                file: '/tmp/models/whisper.yaml',
                message: 'no such decode',
                line: null,
                column: null,
              },
            ],
          },
        ]}
      />,
    )

    expect(screen.getByText('/tmp/models/qwen3.yaml:14:3')).toBeInTheDocument()
    // No position is no colons, rather than a `:0:0` that points at nothing.
    expect(screen.getByText('/tmp/models/whisper.yaml')).toBeInTheDocument()
  })

  it('folds away, because a bar you cannot get rid of is a bar you stop reading', async () => {
    const user = userEvent.setup()
    render(<ConfigBanner directories={[directory('models', 1, ['unknown field `templat`'])]} />)

    await user.click(screen.getByRole('button', { name: 'Hide' }))

    expect(screen.queryByText('unknown field `templat`')).not.toBeInTheDocument()
    // The count stays: folding away the detail is not folding away the news.
    expect(screen.getByText('1 file of the configuration did not load')).toBeInTheDocument()
  })

  /**
   * A save that lands while the bar is folded is exactly the moment to unfold
   * it: what was hidden was the previous complaint, not this one.
   */
  it('opens itself again when a later save says something new', async () => {
    const user = userEvent.setup()
    const page = render(
      <ConfigBanner directories={[directory('models', 1, ['unknown field `templat`'])]} />,
    )

    await user.click(screen.getByRole('button', { name: 'Hide' }))
    page.rerender(<ConfigBanner directories={[directory('mcp', 0, ['duplicate stage `prod`'])]} />)

    expect(screen.getByText('duplicate stage `prod`')).toBeInTheDocument()
  })

  it('stays folded while the same files are still broken', async () => {
    const user = userEvent.setup()
    const broken = [directory('models', 1, ['unknown field `templat`'])]
    const page = render(<ConfigBanner directories={broken} />)

    await user.click(screen.getByRole('button', { name: 'Hide' }))
    // A re-render for another reason — a model selected, a run finishing — is
    // not a reason to reopen what was deliberately put away.
    page.rerender(<ConfigBanner directories={[...broken]} />)

    expect(screen.queryByText('unknown field `templat`')).not.toBeInTheDocument()
  })
})
