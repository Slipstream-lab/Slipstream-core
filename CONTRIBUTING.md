# Contributing to Slipstream Core

Thanks for your interest in contributing. Slipstream Core is the analytical
credibility layer of the project: correctness, determinism, and clarity matter
more than speed or cleverness.

## Ground rules

- Never fabricate analytical results to make a test pass. If a feature is not
  yet implemented, expose a clean interface, an explicit `TODO`, and tests that
  pin the intended behavior.
- Never fabricate benchmark numbers. Only report numbers you actually measured.
- Never commit secrets, keys, tokens, or credentials.
- Prefer deterministic behavior. Every algorithm in this workspace must produce
  identical output for identical input on every platform.

## Development workflow

1. Fork or branch from `main`.
2. Write or update tests alongside your change.
3. Run the full validation suite:

   ```sh
   cargo fmt --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace
   cargo build --workspace
   ```

4. Commit with a conventional commit message, e.g.
   `feat(scheduler): add greedy stage assignment`.
5. Open a pull request against `main` and reference the issue it resolves
   (e.g. `Closes #123`).

## Commit message style

Use [Conventional Commits](https://www.conventionalcommits.org/):

- `feat(<crate>): ...` for new functionality
- `fix(<crate>): ...` for bug fixes
- `refactor(<crate>): ...` for behavior-preserving changes
- `docs(<area>): ...` for documentation
- `test(<crate>): ...` for test additions
- `chore(<area>): ...` for tooling/build/maintenance

## Crate boundaries

- `slipstream-footprint` depends on nothing inside the workspace.
- `slipstream-scheduler` and `slipstream-score` depend on `footprint`.
- `slipstream-replay` depends on `footprint`, `scheduler`, and `score`.
- `slipstream-cli` depends on all of the above.

Keep dependencies pointing in this direction; do not introduce cycles.
