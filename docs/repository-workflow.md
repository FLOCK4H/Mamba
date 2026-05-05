# Repository Workflow

## Source-of-truth files

This repository is governed by checked-in process files, not only by code:

| File | Purpose |
| --- | --- |
| `AGENTS.md` | Repository-wide contributor rules, safety model, required workflow |
| `PRD.md` | Active roadmap and milestone framing |
| `STATUS.md` | Append-only work log with testing and follow-ups |
| `FINDINGS.md` | Append-only operational knowledge base |
| `UPSTREAM_SOURCES.lock` | Snapshot of synced protocol references |

## Required flow for protocol work

1. Read `PRD.md` and relevant modules.
2. Run `scripts/sync_sources.sh` before relying on upstream protocol references.
3. Implement the current milestone rather than opportunistic cleanup.
4. Validate in the host runtime path.
5. Update `FINDINGS.md` incrementally as discoveries happen.
6. Finish by appending a `STATUS.md` entry with evidence and follow-ups.

## Runtime safety

- Default behavior is build-only or simulated.
- Mainnet sends are manual-only unless explicitly unlocked by the user.
- Devnet live validation is allowed when required, but the cluster must be verified first.
- Spend-cap checks are not optional.
- Secrets and private keys must never be committed.

## UI evidence rule

For user-facing TUI work, the repo requires screenshot or snapshot evidence in `artifacts/`. This documentation pass uses deterministic snapshot output from:

```bash
cargo run --bin mamba -- --snapshot
```

## Documentation rule

Implementation and validation come first. Documentation is the final step after code changes are validated. This site follows that rule by documenting the runtime, scripts, tracked evidence, and repository inventory after the cleaner feature and snapshot evidence were in place.
