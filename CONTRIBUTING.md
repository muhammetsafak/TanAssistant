# Contributing to Tan

Thank you for your interest in contributing. This guide explains how to propose changes.

## Before You Start

- Small fixes (typos, broken links, obvious bugs) can be sent directly as a pull request.
- For a new feature or a change in behavior, open an issue first so the approach can be discussed before you write code.
- Opening a pull request does not guarantee that it will be accepted.
- Do not report security vulnerabilities in issues or pull requests; follow the [security policy](SECURITY.md).

## Development Setup

Requirements:

- Rust (stable) with `rustfmt` and `clippy`
- Node.js 22+ and npm
- CMake and libclang, plus the platform packages listed in [Development](docs/development.md#prerequisites)

Clone the repository and install dependencies:

```bash
git clone https://github.com/muhammetsafak/TanAssistant.git
cd TanAssistant
npm --prefix ui ci
./ui/node_modules/.bin/tauri dev
```

## Running Tests

These are the same commands that CI runs. Run both blocks before opening a pull request;
`scripts/ci-local.sh all` runs the macOS and Linux CI jobs on your machine.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check-deps.sh
npm --prefix ui run lint
```

```bash
cargo test --workspace
npm --prefix ui test -- --run
npm --prefix ui run build
```

## Commit Messages

This project follows [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):

```text
<type>(<scope>)!: <description>

<body>
```

- **Types:** `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- **Scope** is optional, lowercase kebab-case, and names a module or area: `feat(auth): ...`.
- **Description** is in English, imperative mood, starts with a lowercase letter, has no trailing period and is at most 72 characters.
- **Body** is optional and explains why the change was made. Wrap lines at 72 characters.
- Mark a breaking change only with `!` after the type or scope, and describe it in the body.
- Do not add footers or trailers (`BREAKING CHANGE:`, `Refs:`, `Closes #12`, `Co-Authored-By:`, `Signed-off-by:` or any tool attribution). Link issues in the pull request description instead.
- Keep one logical change per commit.

Example:

```text
fix(api): return 404 for unknown project slug
```

## Pull Requests

1. Create a branch from `main` named `<type>/<short-kebab-description>`, for example `feat/token-refresh` or `fix/123-null-slug-404`.
2. Write the pull request title in Conventional Commits format. Pull requests are squash merged, so the title becomes the commit message on `main`.
3. If the change affects users, add an entry under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md).
4. Fill in the pull request template and link the related issue with `Closes #<number>`.
5. Make sure CI passes. A maintainer reviews the pull request and may ask for changes before merging.

## Code of Conduct

This project is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to uphold it.
