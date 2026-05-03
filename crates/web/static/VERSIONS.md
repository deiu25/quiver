# Vendored static assets

Pinned upstream versions and SHA-256 hashes. Bumping an asset is a four-step
operation:

1. Download the new release into `static/` overwriting the old file.
2. Run `sha256sum static/<file>` and update the hash below.
3. Update the version line.
4. Re-run `cargo test -p toolhub-web` and exercise the affected pages manually.

| Asset           | Version | Source                                                     | SHA-256                                                              |
| --------------- | ------- | ---------------------------------------------------------- | -------------------------------------------------------------------- |
| `htmx.min.js`   | 2.0.4   | https://unpkg.com/htmx.org@2.0.4/dist/htmx.min.js          | `e209dda5c8235479f3166defc7750e1dbcd5a5c1808b7792fc2e6733768fb447`   |
| `htmx-sse.js`   | 2.2.2   | https://unpkg.com/htmx-ext-sse@2.2.2/sse.js                | `83eca6fa0611fe2b0bf1700b424b88b5eced38ef448ef9760a2ea08fbc875611`   |
