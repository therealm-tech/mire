import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { ModelSummary } from '../api'
import { ModelList } from './ModelList'

const PLAIN: ModelSummary = {
  id: 'nomic',
  name: 'nomic',
  isDefault: true,
  kind: 'embedding',
  url: 'http://127.0.0.1:11434/api/embed',
  auth: null,
  source: '/tmp/nomic.yaml',
  hasPrompt: true,
  hasDecode: true,
  requiresUpload: false,
}

/** One file, two stages: two endpoints under the one name they share. */
const STAGED: ModelSummary[] = [
  {
    ...PLAIN,
    id: 'qwen3@gateway',
    name: 'qwen3',
    stage: 'gateway',
    isDefault: false,
    kind: 'chat',
    url: 'http://127.0.0.1:11435/v1/chat/completions',
    source: '/tmp/qwen3.yaml',
  },
  {
    ...PLAIN,
    id: 'qwen3@ollama',
    name: 'qwen3',
    stage: 'ollama',
    isDefault: true,
    kind: 'chat',
    url: 'http://127.0.0.1:11434/api/chat',
    source: '/tmp/qwen3.yaml',
  },
]

function list(selected: string | null, stages: Record<string, string> = {}, onSelect = vi.fn()) {
  render(
    <ModelList
      models={[PLAIN, ...STAGED]}
      selected={selected}
      stages={stages}
      onSelect={onSelect}
    />,
  )
  return onSelect
}

describe('ModelList', () => {
  it('gives a file one row, whatever the number of stages under it', () => {
    list(null)

    expect(screen.getAllByRole('listitem')).toHaveLength(2)
    // The name, and nowhere the `name@stage` the call carries: picking the
    // endpoint and picking where it runs are two questions now.
    expect(screen.getByText('qwen3')).toBeInTheDocument()
    expect(screen.queryByText(/qwen3@/)).not.toBeInTheDocument()
  })

  it('picks the stage that is pressed rather than the row it sits on', async () => {
    const user = userEvent.setup()
    const onSelect = list('qwen3@ollama')

    await user.click(screen.getByRole('button', { name: /gateway/ }))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'qwen3@gateway' }))
  })

  it('shows the URL of the stage that is selected, since that is what Send asks', () => {
    list('qwen3@gateway')

    expect(screen.getByText('http://127.0.0.1:11435/v1/chat/completions')).toBeInTheDocument()
    expect(screen.queryByText('http://127.0.0.1:11434/api/chat')).not.toBeInTheDocument()
  })

  it('takes the name alone for the default stage, from wherever the selection was', async () => {
    const user = userEvent.setup()
    const onSelect = list('nomic')

    // The row of a model nobody has picked stands for the entry a bare `qwen3`
    // means — its URL is on the row, and pressing the name asks for it. Coming
    // to a staged model is one click, not a click and then a stage.
    expect(screen.getByText('http://127.0.0.1:11434/api/chat')).toBeInTheDocument()

    // The card is what is pressed, as it is on a model with no stage at all —
    // the URL included, which is inside the row's button rather than beside it.
    await user.click(screen.getByText('http://127.0.0.1:11434/api/chat'))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'qwen3@ollama' }))
  })

  it('comes back to the stage last picked rather than to the default', async () => {
    const user = userEvent.setup()
    const onSelect = list('nomic', { qwen3: 'gateway' })

    // The point of stages is that the question was asked somewhere in
    // particular, so coming back to a model comes back there.
    expect(screen.getByText('http://127.0.0.1:11435/v1/chat/completions')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /^qwen3 chat/ }))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'qwen3@gateway' }))
  })

  it('falls back to the default when the remembered stage is gone from the file', async () => {
    const user = userEvent.setup()
    // `preprod` was renamed or dropped since it was last picked: nothing matches
    // it, and a row pointing at nothing would be a model you cannot press.
    const onSelect = list('nomic', { qwen3: 'preprod' })

    expect(screen.getByText('http://127.0.0.1:11434/api/chat')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /^qwen3 chat/ }))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'qwen3@ollama' }))
  })

  /**
   * The list never shows a stage as the one a bare name means unless the server
   * said so, and the server refuses a file that leaves the choice open — but a
   * row still has to point somewhere rather than at nothing.
   */
  it('points at the first stage when no entry claims to be the default', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn()
    const orphaned = STAGED.map((model) => ({ ...model, isDefault: false }))
    render(<ModelList models={orphaned} selected={null} stages={{}} onSelect={onSelect} />)

    await user.click(screen.getByRole('button', { name: /^qwen3 chat/ }))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'qwen3@gateway' }))
  })

  it('marks the default stage for a reader and for a screen reader', () => {
    list(null)

    const stages = screen.getByRole('group', { name: 'Stages of qwen3' })
    expect(within(stages).getByRole('button', { name: /ollama/ })).toHaveAccessibleName(
      'ollama (default)',
    )
    expect(within(stages).getByRole('button', { name: /gateway/ })).toHaveAccessibleName('gateway')
  })

  it('leaves a file that declares no stage exactly as it was', async () => {
    const user = userEvent.setup()
    const onSelect = list(null)

    expect(screen.queryByRole('group', { name: 'Stages of nomic' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /^nomic embedding/ }))

    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'nomic' }))
  })
})
