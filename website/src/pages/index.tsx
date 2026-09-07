import Link from '@docusaurus/Link'
import useBaseUrl from '@docusaurus/useBaseUrl'
import useDocusaurusContext from '@docusaurus/useDocusaurusContext'
import Layout from '@theme/Layout'
import type { ReactNode } from 'react'
import styles from './index.module.css'

// `noUncheckedIndexedAccess` makes a CSS-module lookup `string | undefined`,
// and `exactOptionalPropertyTypes` refuses to pass that to a `className?:
// string`. A missing class is an empty one.
const css = (name: keyof typeof styles): string => styles[name] ?? ''

type Answer = {
  readonly question: string
  readonly body: string
  readonly to: string
  readonly cta: string
}

const ANSWERS: readonly Answer[] = [
  {
    question: 'Does the endpoint answer?',
    body: 'One YAML file per endpoint: the URL, the body template, the credential. Save it and the next call uses it — no restart, and no button that writes into your configuration behind your back.',
    to: '/docs/guides/models',
    cta: 'Writing a model',
  },
  {
    question: 'Is the auth actually enforced?',
    body: 'Static tokens, OIDC workload identities and browser logins, resolved in the process against the URL the request will really use. A 401 from anonymous is a passing result.',
    to: '/docs/guides/auth',
    cta: 'Credentials',
  },
  {
    question: 'Is the response shaped the way you expect?',
    body: 'A decode cascade of JSONPaths, or a named built-in shape. Decoding never fails a call: a miss is a trace entry beside the raw body, so you can see what was looked for and what was not there.',
    to: '/docs/guides/models',
    cta: 'Decode cascades',
  },
  {
    question: 'Does tool calling work?',
    body: 'Simulated tools prove the model emits well-formed calls. Real MCP servers, switched on per run, prove the whole thing — with hooks around each call and every way out of the loop named.',
    to: '/docs/guides/mcp',
    cta: 'MCP servers',
  },
]

function Hero(): ReactNode {
  const logo = useBaseUrl('img/mire-mark.svg')

  return (
    <header className={css('hero')}>
      <div className="container">
        <img className={css('mark')} src={logo} alt="" width={72} height={72} />
        <h1 className={css('title')}>mire</h1>
        <p className={css('tagline')}>
          A test pattern for model endpoints. You put a known signal in, and you look at what comes
          out.
        </p>
        <div className={css('actions')}>
          <Link className="button button--primary button--lg" to="/docs/getting-started/first-call">
            Your first call
          </Link>
          <Link className="button button--secondary button--lg" to="/docs/intro">
            Read the documentation
          </Link>
        </div>
      </div>
    </header>
  )
}

function Answers(): ReactNode {
  return (
    <section className={css('answers')}>
      <div className="container">
        <div className={css('grid')}>
          {ANSWERS.map((answer) => (
            <article className={css('card')} key={answer.question}>
              <h2 className={css('question')}>{answer.question}</h2>
              <p className={css('body')}>{answer.body}</p>
              <Link className={css('cardLink')} to={answer.to}>
                {answer.cta} →
              </Link>
            </article>
          ))}
        </div>
      </div>
    </section>
  )
}

function Shape(): ReactNode {
  return (
    <section className={css('shape')}>
      <div className="container">
        <h2 className={css('shapeTitle')}>One process, on your machine</h2>
        <p className={css('shapeBody')}>
          <code>mire</code> listens on loopback, serves the UI and an HTTP API, and makes every
          outbound call itself. Because the calls originate in the process rather than in the tab,
          there is no CORS to fight, a workload identity is testable, and there is exactly one place
          a credential can leak.
        </p>
        <p className={css('shapeBody')}>
          It is not a gateway, not a proxy you put in front of anything, and not a deployment.
          Nothing is stored between runs, and the only thing it writes to disk is a file you
          attached.
        </p>
        <div className={css('actions')}>
          <Link className="button button--secondary" to="/docs/internals/architecture">
            How it is put together
          </Link>
        </div>
      </div>
    </section>
  )
}

export default function Home(): ReactNode {
  const { siteConfig } = useDocusaurusContext()

  return (
    <Layout title={siteConfig.title} description={siteConfig.tagline}>
      <Hero />
      <main>
        <Answers />
        <Shape />
      </main>
    </Layout>
  )
}
