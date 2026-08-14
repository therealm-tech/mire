/**
 * A model's answer, read as the markdown it almost always is.
 *
 * Endpoints answer in markdown whether or not anyone asked them to — headings,
 * bullets, fenced code — and a `whitespace-pre-wrap` block shows you the
 * asterisks and the backticks instead of what they meant. This renders them,
 * which is the difference between reading an answer and decoding one.
 *
 * The raw text has not gone anywhere: what the endpoint sent is a card down in
 * the traffic panel, byte for byte. This is the reading view, and it is only
 * ever pointed at the model — a question is typed, a tool result is JSON, and
 * neither is prose that wants a heading level.
 *
 * `react-markdown` builds React elements rather than an HTML string, so nothing
 * here goes near `dangerouslySetInnerHTML` and a `<script>` in an answer is text
 * about a script. Link targets go through the library's own URL filter, which
 * keeps `javascript:` and `data:` out of an `href` — worth having, given the
 * href in question was written by something on the other end of a socket.
 */

import { createContext, type ReactNode, useContext } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'

/**
 * Whether the `<code>` being rendered is the inside of a fence.
 *
 * Both spellings arrive at the same component, and only the tree says which is
 * which: a fenced block is always `pre > code`, an inline span never is. The
 * language class would be a cheaper test and a wrong one — ```` ``` ```` with no
 * language after it is a block that carries no class at all.
 */
const Fenced = createContext(false)

/** Both spellings of `<code>`, told apart by where in the tree they landed. */
function CodeSpan({ children }: { children?: ReactNode }) {
  return useContext(Fenced) ? (
    <code className="font-mono text-xs leading-relaxed">{children}</code>
  ) : (
    <code className="rounded bg-well px-1 py-0.5 font-mono text-[0.9em]">{children}</code>
  )
}

const COMPONENTS: Components = {
  // Heading levels are relative, not absolute: this sits inside a chat bubble,
  // and an `h1` at `h1` size would out-shout the panel it is printed in.
  h1: ({ children }) => <h3 className="font-semibold text-base">{children}</h3>,
  h2: ({ children }) => <h4 className="font-semibold text-sm">{children}</h4>,
  h3: ({ children }) => <h5 className="font-semibold text-sm">{children}</h5>,
  h4: ({ children }) => <h6 className="font-semibold text-sm">{children}</h6>,
  h5: ({ children }) => <h6 className="font-medium text-sm">{children}</h6>,
  h6: ({ children }) => <h6 className="font-medium text-muted text-sm">{children}</h6>,

  // `break-words`: a URL or a base64 blob with no space in it is not a reason
  // for the page to grow sideways.
  p: ({ children }) => <p className="break-words">{children}</p>,

  ul: ({ children }) => <ul className="list-disc space-y-1 pl-5">{children}</ul>,
  ol: ({ children }) => <ol className="list-decimal space-y-1 pl-5">{children}</ol>,
  li: ({ children }) => <li className="break-words">{children}</li>,

  blockquote: ({ children }) => (
    <blockquote className="space-y-2 border-line border-l-2 pl-3 text-muted">{children}</blockquote>
  ),

  // Answers cite things, and a citation you cannot click is a citation you
  // retype. Away from the app, because leaving mid-run would lose the run.
  a: ({ href, children }) => (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      className="break-all underline underline-offset-2 hover:text-muted"
    >
      {children}
    </a>
  ),

  hr: () => <hr className="border-line" />,

  code: CodeSpan,

  // The block chrome lives here rather than on the `<code>` inside it, so that
  // the scroll container is the box and not its contents.
  pre: ({ children }) => (
    <Fenced value={true}>
      <pre className="overflow-x-auto rounded bg-well p-2">{children}</pre>
    </Fenced>
  ),

  // A wide table is the one thing here that cannot be made to wrap, so it gets
  // its own scroll rather than pushing the conversation off the screen.
  table: ({ children }) => (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-xs">{children}</table>
    </div>
  ),
  th: ({ children }) => (
    <th className="border border-line px-2 py-1 text-left font-semibold">{children}</th>
  ),
  td: ({ children }) => <td className="border border-line px-2 py-1 align-top">{children}</td>,

  img: ({ src, alt }) => <img src={src} alt={alt ?? ''} className="max-w-full rounded" />,
}

/**
 * Render `text` as markdown.
 *
 * `space-y-2` on the wrapper rather than margins on the blocks: the gaps then
 * belong to the answer as a whole, and a one-paragraph reply is a bubble with no
 * stray leading under it.
 */
export function Markdown({ children }: { children: string }): ReactNode {
  return (
    <div className="space-y-2">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>
        {children}
      </ReactMarkdown>
    </div>
  )
}
