# Repository Workflow

## Source-of-truth files

| File | Purpose |
|------|---------|
| `AGENTS.md` | Contributor rules, safety model, required workflow |
| `PRD.md` | Active roadmap and milestone framing |
| `STATUS.md` | Append-only work log with testing evidence and follow-ups |
| `FINDINGS.md` | Append-only operational knowledge base (blockers, workarounds, procedures) |
| `UPSTREAM_SOURCES.lock` | Snapshot of synced protocol references |

## Protocol work checklist

1. Read `PRD.md` and the relevant market/module code.
2. Run `scripts/sync_sources.sh` before relying on upstream protocol references.
3. Implement the current milestone. Avoid opportunistic cleanup outside the active scope.
4. Validate through the host runtime path (`cargo run --bin mamba_api`).
5. Append findings to `FINDINGS.md` as they happen, not at the end.
6. Finish by appending a `STATUS.md` entry with evidence and follow-ups.

## Runtime safety

| Rule | Detail |
|------|--------|
| Build-first by default | Builders return unsigned transactions. Execute routes exist separately. |
| Mainnet sends are manual | Locked unless explicitly enabled in runtime configuration. |
| Devnet validation is allowed | Required cluster verification before sending. |
| Spend-cap checks are mandatory | Cannot be disabled or bypassed. |
| No secrets in git | `.env` and private keys must never be committed. |

## UI evidence

For TUI-facing work, the repo requires screenshot or snapshot evidence in `artifacts/`. Generate deterministic snapshots with:

```bash
cargo run --bin mamba -- --snapshot
```

## Documentation policy

Implementation and validation come first. Documentation is the final step after code changes are verified.
