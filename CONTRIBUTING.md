# Contributing

Issues and pull requests go to
[therealm-tech/mire](https://github.com/therealm-tech/mire). The short version:
install the toolchains below, `pre-commit install`, and make sure
`pre-commit run --all-files`, `cargo test` and `npm --prefix ui test` are all
green before you push.

See [README.md](README.md#getting-started) for running the project itself, and
[ARCHITECTURE.md](ARCHITECTURE.md) for how it is put together.

## Development setup

### Toolchains

- **Rust** — the channel and components are pinned in
  [`rust-toolchain.toml`](rust-toolchain.toml), so `rustup` installs the right
  ones with no argument. Get `rustup` from [rustup.rs](https://rustup.rs), then:

  ```sh
  rustup toolchain install
  ```

- **Node.js ≥ 24** and `npm`, for the front end — see
  [nodejs.org](https://nodejs.org/en/download).

  ```sh
  npm --prefix ui ci
  ```

- **`pre-commit`**, which is the lint gate:

  ```sh
  pipx install pre-commit
  pre-commit install
  ```

### The binaries the hooks shell out to

Four hooks are `language: system`, so the tool has to be on your `PATH` before
`pre-commit` runs. Missing one shows up as a cryptic hook failure rather than a
helpful message, so install them first. The versions are the ones CI pins and the
repository was last verified against.

**macOS**

```sh
brew install hadolint trivy shellcheck actionlint
```

**Linux** — the release tarballs, which is what
[`.github/workflows/quality.yaml`](.github/workflows/quality.yaml) installs:

```sh
sudo curl -fsSL -o /usr/local/bin/hadolint https://github.com/hadolint/hadolint/releases/download/v2.14.0/hadolint-Linux-x86_64
sudo chmod +x /usr/local/bin/hadolint
curl -fsSL https://github.com/aquasecurity/trivy/releases/download/v0.70.0/trivy_0.70.0_Linux-64bit.tar.gz | sudo tar -xz -C /usr/local/bin trivy
curl -fsSL https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.linux.x86_64.tar.xz | sudo tar -xJ --strip-components=1 -C /usr/local/bin shellcheck-v0.11.0/shellcheck
curl -fsSL https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz | sudo tar -xz -C /usr/local/bin actionlint
```

`ruff`, which lints [`dev/mcp/server.py`](dev/mcp/server.py), needs nothing:
`pre-commit` builds it its own isolated environment.

### That the setup works

```sh
pre-commit run --all-files
```

Green here means every linter the CI runs is happy. Then the two suites below.

### Working on the front end

```sh
npm --prefix ui run dev
```

Vite serves the front end with hot reload and proxies `/api` to a `mire` already
running on its default port.

To see a change in the real binary, `cargo build` is enough:
[`build.rs`](build.rs) runs Vite before compiling, and `npm ci` ahead of it when
the lockfile has moved since the last install. `rust-embed` reads `ui/dist` at
compile time, so the bundle is an input to the build rather than a step to
remember — which is also why a front end that fails to build fails `cargo build`,
Vite's error underneath. In a debug build the assets are read from `ui/dist` at
runtime, so `npm --prefix ui run build` also refreshes a binary that is already
compiled.

`MIRE_BUILD_UI=0` skips all of that, for a build that has a bundle already or
none at all: the image build sets it, because its Node stage produced `ui/dist`
and the Rust stage carries no npm, and so does the `cargo test` job, which needs
no front end and gets the placeholder page the script writes instead.

The palette is the logo and nothing else, and
[`ui/src/index.css`](ui/src/index.css) holds all of it: a brand scale sampled
from the mark, and above it the roles a component names (`paper`, `panel`,
`line`, `ink`, `muted`, `brand`, and the three verdict tones). The stock Tailwind
palette is switched off there, so a stray `text-stone-400` compiles to no utility
at all rather than to a colour that nearly fits. The roles follow
`prefers-color-scheme` on their own, which is why no component carries a `dark:`
variant. The mark itself is drawn in
[`ui/src/components/Mark.tsx`](ui/src/components/Mark.tsx) rather than shipped as
an image: it inherits `currentColor`, so one tag serves both schemes.

Markdown is the one place the front end takes a real dependency.
[`ui/src/components/Markdown.tsx`](ui/src/components/Markdown.tsx) wraps
`react-markdown` and `remark-gfm` and maps every tag to the palette above rather
than pulling in a typography plugin. It costs about 48 kB gzipped in the embedded
bundle — a hand-rolled parser would cost less and be wrong about a corner of
CommonMark nobody would find until a model landed on it.

## Running the tests

Both suites are expected green on every pull request. New behaviour comes with
tests; a fix comes with the regression test that would have caught it.

Neither suite needs a network, a container runtime or the
[dev stack](docs/dev-stack.md): outbound HTTP is served by `wiremock` in-process,
and the front end runs under `jsdom`.

**Rust** — unit tests live beside the code they cover,
[`tests/api.rs`](tests/api.rs) drives the HTTP surface end to end:

```sh
cargo test
```

```sh
cargo test --test api
```

```sh
cargo test decode::paths
```

**Front end** — Vitest with Testing Library:

```sh
npm --prefix ui test
```

```sh
npm --prefix ui test -- src/preflight.test.ts
```

Coverage is available (`npm --prefix ui test -- --coverage`) but no threshold is
enforced, so it is a tool rather than a gate.

Two things the Rust suite deliberately checks, and that are worth keeping green
rather than working around. Outbound HTTP is covered on every failure path that
matters: an expected `401`, a timeout, a malformed body, an empty body, a decode
path that misses, and a cross-host redirect, where the `Authorization` header
must not follow. OIDC runs against a mock identity provider: discovery, caching,
expiry, a rejected cached token refreshed and replayed exactly once, a rotated
service account token, and a failed exchange. And several tests fail if a
credential appears anywhere in an API response — including the case where the
endpoint or the identity provider quotes it back at us in an error message.

## Pre-commit hooks

[`.pre-commit-config.yaml`](.pre-commit-config.yaml) is the whole lint gate, and
CI runs exactly it — nothing is linted in CI that a local run does not lint.

```sh
pre-commit run --all-files
```

```sh
pre-commit run cargo-clippy --all-files
```

| Hook | What it checks | Fixes itself |
| --- | --- | --- |
| `trailing-whitespace`, `end-of-file-fixer` | Whitespace hygiene | yes |
| `check-yaml` | Every YAML file parses | no |
| `check-added-large-files` | A large file added by accident | no |
| `check-merge-conflict` | Conflict markers left in a file | no |
| `detect-private-key` | A private key about to be committed | no |
| `ruff-check`, `ruff-format` | `dev/mcp/server.py` | format only |
| `cargo-fmt` | `cargo fmt --all -- --check` | run `cargo fmt --all` |
| `cargo-clippy` | `--all-targets -D warnings`, with the manifest's `pedantic` lints | no |
| `hadolint` | [`Dockerfile`](Dockerfile) | no |
| `trivy-config` | Container misconfiguration hadolint does not catch, every severity | no |
| `shellcheck` | [`scripts/release.sh`](scripts/release.sh) and any other shell | no |
| `actionlint` | [`.github/workflows/`](.github/workflows/) | no |
| `biome` | Format and lint of `ui/`; `npm --prefix ui run check` writes the fixes | with `check` |
| `ui-typecheck` | `tsc --noEmit`, a blocking gate on the same footing as the linter | no |

The hooks stop short of the test suites on purpose: a commit hook has to stay
fast enough that nobody reaches for `--no-verify`, and a suite is the first thing
to grow past that. Both suites run in CI instead.

**`--no-verify` and `SKIP=` are not the fix.** A red hook is a finding. If the
rule is genuinely wrong for this repository, change the configuration in the same
pull request and say why — what `trivy config` waives lives in
[`.trivyignore.yaml`](.trivyignore.yaml), in one place, with the reasoning
attached.

## Continuous integration

| Workflow | Triggers on | What it does | Reproduce locally |
| --- | --- | --- | --- |
| [`quality.yaml`](.github/workflows/quality.yaml) | every push to `main`, every pull request | `pre-commit run --all-files`, `cargo test`, `npm --prefix ui test`, and Trivy over the repository | `pre-commit run --all-files`, then both suites |
| [`build.yaml`](.github/workflows/build.yaml) | the same, when `src/`, `ui/`, the manifests or the `Dockerfile` change | Builds the image per architecture on its own native runner, scans it, and on `main` pushes it with a multi-arch manifest | `docker build .` |
| [`release.yaml`](.github/workflows/release.yaml) | a `v*` tag | Checks the tag against the manifest version, then publishes the image and the GitHub Release | — |

All four `quality` jobs and both `build` jobs block a merge. The Trivy job runs
twice on purpose: one pass publishes everything actionable to the Security tab as
SARIF, the other fails the build on `HIGH` and `CRITICAL` only.

Neither workflow needs a secret you have to provide: publishing uses the
workflow's own `GITHUB_TOKEN`. A pull request from a fork cannot upload SARIF, so
that step is skipped there and the findings still gate the build through the
second pass.

The one check with no local equivalent is the multi-arch image publish, which
needs the registry.

## Releasing

[`scripts/release.sh`](scripts/release.sh) keeps the tag and the manifest in
step, which is what `release.yaml` verifies before it publishes anything:

```sh
scripts/release.sh 0.4.0
```

It refuses a dirty tree, a branch other than `main`, a branch out of sync with
`origin`, and a tag that already exists; then it bumps
[`Cargo.toml`](Cargo.toml), runs the tests, commits, tags and — after one
confirmation — pushes. `--no-push` stops before the push, `--skip-tests` before
the suite. The tag must match the `version` in [`Cargo.toml`](Cargo.toml): the
release fails on a mismatch rather than publishing a binary that lies about which
release it is.

What comes out: `ghcr.io/therealm-tech/mire:X.Y.Z` and `:latest` as a multi-arch
manifest over `linux/amd64` and `linux/arm64`, and one `.tar.gz` per architecture
on the GitHub Release — the binary and `LICENSE`, nothing else, each with its
`.sha256` published beside it rather than packed inside. The binaries are taken
out of the images rather than built a second time from the same source, so what
you download is what was scanned and published.

## Submitting a change

- Branch off `main`. There is no naming convention to follow.
- **Commit messages** are imperative and lowercase, with an optional `area:`
  prefix — `git log --oneline` is the reference.
- **Update the documentation in the same change as the code.** A new option
  belongs in [README.md](README.md#configuration) as it is added, a new knob in
  the [document that covers it](docs/), and a design change in
  [ARCHITECTURE.md](ARCHITECTURE.md) rewritten to describe the new state.
- **Label the pull request.** The release notes are generated from the labels in
  [`.github/release.yml`](.github/release.yml) — `security`, `feature`,
  `enhancement`, `bug`, `fix`, `models`, `config`, `documentation`,
  `dependencies` — and an unlabelled pull request lands under "Other changes".
- CI must be green.
