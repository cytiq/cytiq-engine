# Repository Guidelines

Contributor guidance for this repository lives in [CONTRIBUTING.md](./CONTRIBUTING.md).

## Agent tooling

- Follow TigerStyle priorities: Safety → Performance → Developer Experience.
- For code exploration and edits, use [token-efficient-coding](.agents/skills/token-efficient-coding/SKILL.md).
- Use `rg` for exact text, Serena MCP for symbols and references, and `grepai` CLI for semantic search.
  If an optional tool is unavailable, continue with `rg` and bounded file reads.
- Read [agent tooling](docs/agent-tools.md) only for setup, indexing, memory or architecture audits.

## Session artifacts

Keep session notes, workspace snapshots, implementation plans, ad-hoc audits and
benchmark/rollout run reports local under ignored `target/` or `.serena/` paths.
Do not stage or force-add them unless the user explicitly requests those exact
artifacts. `docs/` is for maintained product, API, setup and migration documentation.

## Cytiq wiki

When a change affects features, public contracts, compatibility, setup, deployment, or operations, update the
relevant pages in [cytiq-wiki](https://github.com/cytiq/cytiq-wiki) as part of the same change. Document affected
version pins, migrations, and verification steps where applicable. Update the repository index and navigation
when adding a repository or guide.
