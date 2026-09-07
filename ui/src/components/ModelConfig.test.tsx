import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import type { ModelConfig as Config } from '../api'
import { ModelConfig } from './ModelConfig'

/** A staged chat model, as the API answers it: resolved, not copied. */
const CONFIG: Config = {
  name: 'qwen3',
  stage: 'gateway',
  request: {
    template: '{"model": "qwen3:0.6b", "messages": {{ messages | tojson }}, "max_tokens": 256}',
  },
  decode: {
    from: ['openai-chat', 'ollama-native-chat'],
    content: ['$.choices[0].message.content', '$.message.content'],
    delta: [],
    tool_calls: [],
    finish_reason: ['$.choices[0].finish_reason'],
    usage: [],
    error: [],
    vectors: [],
  },
  agent: null,
}

const DOCUMENT = 'name: qwen3\nkind: chat\nurl: http://127.0.0.1:11435/v1/chat/completions\n'

function show(config: Config | null = CONFIG, document: string | null = DOCUMENT) {
  render(<ModelConfig id="qwen3@gateway" config={config} document={document} error={null} />)
}

describe('ModelConfig', () => {
  it('shows the template as it will actually go out', () => {
    show()

    expect(screen.getByText(/"max_tokens": 256/)).toBeInTheDocument()
    expect(screen.getByText('request.template')).toBeInTheDocument()
  })

  it('lists a flattened cascade in the order it is tried, and says where it came from', () => {
    show()

    expect(screen.getByText('from: openai-chat, ollama-native-chat')).toBeInTheDocument()

    // Both paths of the same field, first one first: which of them wins is the
    // whole question a decode trace answers, and the order is the answer's half.
    const content = screen.getByText('content').parentElement
    expect(content?.textContent).toContain('$.choices[0].message.content')
    expect(content?.textContent).toContain('$.message.content')
    expect(content?.textContent?.indexOf('$.choices')).toBeLessThan(
      content?.textContent?.indexOf('$.message.content') ?? 0,
    )
  })

  it('answers about the loop even when the model declares no agent block', () => {
    show()

    expect(screen.getByText(/not declared/)).toBeInTheDocument()
    // What the loop does anyway, which is the question — an empty block would
    // leave "so what happens?" unanswered.
    expect(screen.getByText('a turn asks for no tool')).toBeInTheDocument()
    expect(screen.getByText('10 turns')).toBeInTheDocument()
  })

  it('reads the stop conditions a declared agent block sets', () => {
    show({
      ...CONFIG,
      agent: {
        stop_when: {
          no_tool_calls: false,
          finish_reason_in: ['stop', 'end_turn'],
          repeated_call: true,
        },
        default_max_turns: 6,
        max_duration_ms: 600000,
      },
    })

    expect(screen.queryByText('a turn asks for no tool')).not.toBeInTheDocument()
    expect(screen.getByText('finish_reason is stop or end_turn')).toBeInTheDocument()
    expect(
      screen.getByText('the same tool is called twice with the same arguments'),
    ).toBeInTheDocument()
    expect(screen.getByText('6 turns')).toBeInTheDocument()
    expect(screen.getByText('600s of wall clock')).toBeInTheDocument()
  })

  it('says when nothing reads the response yet', () => {
    show({ ...CONFIG, decode: undefined })

    expect(screen.getByText('nothing configured')).toBeInTheDocument()
  })

  it('hands over the file to paste', async () => {
    show()
    await userEvent.click(screen.getByRole('tab', { name: 'YAML' }))

    expect(screen.getByText(/url: http:\/\/127\.0\.0\.1:11435/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy YAML' })).toBeInTheDocument()
  })

  it('waits rather than showing an empty model', () => {
    show(null, null)

    expect(screen.getByRole('status')).toHaveTextContent('Reading the configuration…')
    expect(screen.queryByRole('button', { name: 'Copy YAML' })).not.toBeInTheDocument()
  })
})
