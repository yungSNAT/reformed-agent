---
base: &b
  author: anchored
  license: MIT
extra: &e
  author: later-source
  version: 2.0
name: merge
description: YAML merge key
<<: [*b, *e]
license: Apache-2.0
---
Merged frontmatter keeps explicit keys and earlier merge sources.
