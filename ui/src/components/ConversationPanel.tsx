import type { Message } from '../api'
import { Badge, Panel, type Tone } from './primitives'

const ROLE_TONES: Record<Message['role'], Tone> = {
  system: 'warn',
  user: 'neutral',
  assistant: 'good',
  tool: 'warn',
}

/**
 * The conversation as it will go on the wire.
 *
 * Not a chat window with a transcript beside it — this *is* the `messages` array
 * the next request will carry, which is why a turn can be removed. Editing the
 * history and sending again is how you ask "does it still answer that way if the
 * third turn never happened?", and that question is the whole point of the tool.
 */
export function ConversationPanel({
  messages,
  busy,
  onRemove,
  onReset,
}: {
  messages: Message[]
  busy: boolean
  onRemove: (index: number) => void
  onReset: () => void
}) {
  if (messages.length === 0) {
    return null
  }

  return (
    <Panel
      title="Conversation"
      actions={
        <div className="flex items-center gap-2">
          <Badge tone="neutral">
            {messages.length} {messages.length === 1 ? 'turn' : 'turns'}
          </Badge>
          <button
            type="button"
            disabled={busy}
            onClick={onReset}
            className="rounded border border-stone-300 px-2 py-1 text-xs disabled:opacity-50 hover:bg-stone-100 dark:border-stone-700 dark:hover:bg-stone-800"
          >
            New conversation
          </button>
        </div>
      }
    >
      <ol className="space-y-2">
        {messages.map((message, index) => (
          <li
            // The array is the identity here: two turns can carry the same role
            // and the same text, and re-ordering is not a thing that happens.
            // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity of a turn
            key={index}
            className="rounded border border-stone-200 p-2 dark:border-stone-800"
          >
            <div className="mb-1 flex items-center justify-between gap-2">
              <Badge tone={ROLE_TONES[message.role]}>{message.role}</Badge>
              <button
                type="button"
                disabled={busy}
                onClick={() => onRemove(index)}
                aria-label={`Remove turn ${index + 1}`}
                className="text-stone-500 text-xs disabled:opacity-50 hover:underline dark:text-stone-400"
              >
                Remove
              </button>
            </div>

            {message.content ? (
              <p className="whitespace-pre-wrap text-sm">{message.content}</p>
            ) : (
              <p className="text-stone-500 text-sm italic dark:text-stone-400">no text content</p>
            )}

            {message.toolCalls && message.toolCalls.length > 0 ? (
              <div className="mt-2 space-y-1">
                {message.toolCalls.map((toolCall, position) => (
                  <p
                    // biome-ignore lint/suspicious/noArrayIndexKey: same reasoning as the turn above
                    key={position}
                    className="font-mono text-[11px] text-stone-600 dark:text-stone-400"
                  >
                    {toolCall.name}({JSON.stringify(toolCall.arguments)})
                  </p>
                ))}
                <p className="text-amber-700 text-xs dark:text-amber-500">
                  Chat mode does not answer tool calls. Most endpoints refuse the next turn until
                  this one has a result — “Run agent” is the loop that provides it.
                </p>
              </div>
            ) : null}
          </li>
        ))}
      </ol>
    </Panel>
  )
}
