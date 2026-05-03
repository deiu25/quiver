# doc-only fixture

Phase 5 fixture for the doc-fallback path of detect_repo_type. No SKILL.md,
no marketplace.json, no package.json — should land in `RepoType::Doc` and
ingest as a single `type=doc` tool with the README body as long_description.
