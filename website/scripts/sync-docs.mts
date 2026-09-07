// Assembles `website/docs/` — the tree Docusaurus builds from — out of the two
// places the documentation actually lives: the repository's own markdown
// (`docs/`, `ARCHITECTURE.md`, `CONTRIBUTING.md`), and the pages written for
// the site alone (`website/content/`). The output is generated and ignored by
// git; editing it is editing the wrong file.

import { existsSync, statSync } from 'node:fs'
import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const websiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repoRoot = path.resolve(websiteRoot, '..')
const contentDir = path.join(websiteRoot, 'content')
const outDir = path.join(websiteRoot, 'docs')

const repository = 'https://github.com/therealm-tech/mire'
const branch = 'main'
const routeBase = '/docs'

/** A repository document, and where it lands under `docs/`. */
type Synced = {
  /** Path relative to the repository root. */
  readonly from: string
  /** Path relative to the generated docs tree. */
  readonly to: string
}

/**
 * Everything the site pulls in from outside `website/`. A document that is not
 * listed here is not published — `docs/README.md` is the one deliberate
 * omission, since the site's own introduction replaces it.
 */
const SYNCED: readonly Synced[] = [
  { from: 'docs/configuration.md', to: 'guides/configuration.md' },
  { from: 'docs/models.md', to: 'guides/models.md' },
  { from: 'docs/auth.md', to: 'guides/auth.md' },
  { from: 'docs/mcp.md', to: 'guides/mcp.md' },
  { from: 'docs/agent-loop.md', to: 'guides/agent-loop.md' },
  { from: 'docs/streaming.md', to: 'guides/streaming.md' },
  { from: 'docs/embeddings.md', to: 'guides/embeddings.md' },
  { from: 'docs/ui.md', to: 'guides/ui.md' },
  { from: 'docs/dev-stack.md', to: 'guides/dev-stack.md' },
  { from: 'docs/api.md', to: 'reference/api.md' },
  { from: 'ARCHITECTURE.md', to: 'internals/architecture.md' },
  { from: 'CONTRIBUTING.md', to: 'project/contributing.md' },
]

/**
 * Brand assets, kept in one place in the repository rather than copied into the
 * site where nobody would notice them drifting.
 */
const ASSETS: readonly Synced[] = [
  { from: 'mire.svg', to: 'static/img/mire.svg' },
  { from: 'mire-mark.svg', to: 'static/img/mire-mark.svg' },
  { from: 'ui/public/favicon.svg', to: 'static/img/favicon.svg' },
]

/** The ADR directory is synced whole, so a new record needs no edit here. */
const ADR_DIR = 'docs/adr'
const ADR_TO = 'internals/decisions'

/**
 * Targets that have no file of their own on the site. The README is the
 * repository's front page and stays there; a document pointing at one of its
 * sections means the page that covers the same ground here.
 */
const ALIASES: ReadonlyMap<string, string> = new Map([
  ['README.md#getting-started', `${routeBase}/getting-started/installation`],
  ['README.md', `${routeBase}/intro`],
  ['docs/README.md', `${routeBase}/intro`],
])

/** Repository path of a synced document, to the route it is served on. */
const routes = new Map<string, string>()

/** Generated file, to the repository path it was generated from. */
const origins = new Map<string, string>()

const route = (to: string): string => `${routeBase}/${to.replace(/\.mdx?$/, '')}`

const blobUrl = (repoPath: string): string => {
  const kind = statSync(path.join(repoRoot, repoPath)).isDirectory() ? 'tree' : 'blob'
  return `${repository}/${kind}/${branch}/${repoPath}`
}

const editUrl = (repoPath: string): string => `${repository}/edit/${branch}/${repoPath}`

/** Every markdown file under `dir`, relative to it, deepest last. */
const markdownIn = async (dir: string): Promise<string[]> => {
  const entries = await readdir(dir, { recursive: true, withFileTypes: true })
  return entries
    .filter((entry) => entry.isFile() && /\.mdx?$/.test(entry.name))
    .map((entry) => path.relative(dir, path.join(entry.parentPath, entry.name)))
    .sort()
}

/**
 * Resolves one markdown link written against `sourceDir` (a repository-relative
 * directory) into something the site can serve: a route when the target is
 * published here, a GitHub URL when it is a file in the repository, and the
 * link untouched when it points somewhere else entirely.
 */
const resolveTarget = (target: string, sourceDir: string): string => {
  if (/^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith('//') || target.startsWith('#')) {
    return target
  }
  // A site-absolute path is what the hand-written pages use; leave it alone.
  if (target.startsWith('/')) {
    return target
  }

  const hash = target.indexOf('#')
  const filePart = hash === -1 ? target : target.slice(0, hash)
  const anchor = hash === -1 ? '' : target.slice(hash)
  const repoPath = path.posix.normalize(path.posix.join(sourceDir, filePart))

  // An alias points at a different document, so the anchor written for the old
  // one is dropped rather than carried onto a heading that does not exist.
  const alias = ALIASES.get(`${repoPath}${anchor}`) ?? ALIASES.get(repoPath)
  if (alias !== undefined) {
    return alias
  }

  const published = routes.get(repoPath)
  if (published !== undefined) {
    return `${published}${anchor}`
  }

  if (existsSync(path.join(repoRoot, repoPath))) {
    return `${blobUrl(repoPath)}${anchor}`
  }

  throw new Error(`link to a file that does not exist: ${target} (from ${sourceDir})`)
}

/** Rewrites every inline link in `markdown`, leaving fenced code blocks alone. */
const rewriteLinks = (markdown: string, sourceDir: string): string =>
  markdown
    .split(/(^```[\s\S]*?^```$)/m)
    .map((chunk, index) =>
      index % 2 === 1
        ? chunk
        : chunk.replace(
            /\]\(([^)\s]+)\)/g,
            (_match, target: string) => `](${resolveTarget(target, sourceDir)})`,
          ),
    )
    .join('')

/** Adds `custom_edit_url` so "Edit this page" points at the real source file. */
const withEditUrl = (markdown: string, repoPath: string): string => {
  const key = `custom_edit_url: ${editUrl(repoPath)}`
  if (markdown.startsWith('---\n')) {
    return markdown.replace('---\n', `---\n${key}\n`)
  }
  return `---\n${key}\n---\n\n${markdown}`
}

const emit = async (to: string, body: string): Promise<void> => {
  const destination = path.join(outDir, to)
  await mkdir(path.dirname(destination), { recursive: true })
  await writeFile(destination, body)
}

const main = async (): Promise<void> => {
  await rm(outDir, { recursive: true, force: true })
  await mkdir(outDir, { recursive: true })

  const adrs = (await markdownIn(path.join(repoRoot, ADR_DIR))).map((name) => ({
    from: path.posix.join(ADR_DIR, name),
    to: path.posix.join(ADR_TO, name === 'README.md' ? 'index.md' : name),
  }))

  const authored = (await markdownIn(contentDir)).map((name) => ({
    from: path.posix.join('website/content', name),
    to: name,
  }))

  const all = [...SYNCED, ...adrs, ...authored]
  for (const { from, to } of all) {
    routes.set(from, route(to))
    origins.set(to, from)
  }

  for (const { from, to } of all) {
    const source = path.join(repoRoot, from)
    const body = await readFile(source, 'utf8')
    await emit(to, withEditUrl(rewriteLinks(body, path.posix.dirname(from)), from))
  }

  // Images and anything else a hand-written page sits next to.
  await cp(contentDir, outDir, {
    recursive: true,
    filter: (entry) => !/\.mdx?$/.test(entry),
  })

  for (const { from, to } of ASSETS) {
    const destination = path.join(websiteRoot, to)
    await mkdir(path.dirname(destination), { recursive: true })
    await cp(path.join(repoRoot, from), destination)
  }

  process.stdout.write(
    `synced ${all.length} documents and ${ASSETS.length} assets into ${path.relative(repoRoot, websiteRoot)}\n`,
  )
}

await main()
