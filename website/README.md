# The documentation site

[Docusaurus](https://docusaurus.io) site published to
<https://therealm-tech.github.io/mire/> by
[`.github/workflows/docs.yaml`](../.github/workflows/docs.yaml) on every push to
`main`.

## Where the content lives

`website/docs/` is **generated and ignored by git**. It is assembled by
[`scripts/sync-docs.mts`](scripts/sync-docs.mts) out of two places, and editing
anything in it edits a file that is about to be overwritten:

| Source | Lands as |
| --- | --- |
| [`docs/`](../docs) | the guides and the HTTP API reference |
| [`ARCHITECTURE.md`](../ARCHITECTURE.md) | Internals → Architecture |
| [`docs/adr/`](../docs/adr) | Internals → Decisions |
| [`CONTRIBUTING.md`](../CONTRIBUTING.md) | Project → Contributing |
| `website/content/` | everything written for the site alone — the introduction, installation, the first call, and the command-line and configuration-key references |

The sync also rewrites every relative link: one pointing at a document the site
publishes becomes a route, and one pointing at a file in the repository becomes
a GitHub URL. A link that resolves to nothing fails the sync, and a route that
resolves to nothing fails the build — which is what keeps the two sources from
drifting apart in silence.

Adding a document to the site means adding it to the manifest at the top of the
sync script and to [`sidebars.ts`](sidebars.ts). A new ADR needs neither: the
decisions directory is synced whole and its sidebar entry is generated.

## Working on it

```sh
npm ci
npm start          # syncs, then serves with hot reload on http://localhost:3000
```

```sh
npm run build      # what CI builds; broken links and anchors fail it
npm run serve      # serve the built site
npm run check      # biome, writing fixes
npm run typecheck  # tsc --noEmit
```

`npm run sync` on its own regenerates `docs/` and the brand assets under
`static/img/` without starting anything.
