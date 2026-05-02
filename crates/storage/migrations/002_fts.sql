-- Full-text search index over tools. See PLAN.md §6.

CREATE VIRTUAL TABLE tools_fts USING fts5(
    name,
    description,
    long_description,
    triggers,
    examples,
    category,
    content='tools',
    content_rowid='rowid'
);
