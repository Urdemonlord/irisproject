# Contributing to IRIS

## Workflow

- Work on a branch off `main` and open a pull request. `main` is the
  deployed branch; nothing lands there without a reviewed PR.
- Keep PRs focused: one change per PR, with tests for new behavior.

## Checks (must pass before pushing)

CI runs three jobs on every push and pull request (`.github/workflows/ci.yml`):

- **Rust** — `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`
- **Web** — `npm ci`, `npm run typecheck`, `npm run build` (in `apps/web`, Node 22)
- **Audit (advisory)** — `cargo audit` and `npm audit --audit-level=high`;
  findings are reported but do not block the pipeline yet

Run the Rust and web checks locally before opening a PR:

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets
npm --prefix apps/web ci && npm --prefix apps/web run typecheck && npm --prefix apps/web run build
```

## Rules

- Never commit secrets: no `.env`, API keys, tokens, passwords, or private
  keys. Copy `.env.example` to `.env` and fill in your own values; `.env*`
  (except `.env.example`) is git-ignored.
- Do not hardcode deployment-specific values (domains, hosts, app names);
  make them configurable via environment or build-time variables.
- Tests use in-memory SQLite and need no live services or secrets.
- If you find a security issue, follow `SECURITY.md` — do not open a public issue.
