# widget-tool

A small command-line helper for generating widgets from JSON specs.

## When to use

- Bootstrap a new widget package
- Convert a spec sheet into source files
- Lint widget specs against the schema
- Render a preview HTML for review

## Examples

```bash
widget-tool gen ./spec.json --out ./build
```

```ts
import { generate } from 'widget-tool'
generate({ name: 'Card', tokens })
```

## Notes

This is a CLI tool, packaged as a single static binary.
