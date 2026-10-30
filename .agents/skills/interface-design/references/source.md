# Source record

Adapted from the user-supplied `tuara-design/SKILL.md`, selected for generalization
on October 29, 2026. Original SHA-256:

`951e7de167b235a5bfb0d3fc0ac1fffa3490f53715926045dda2ec9620901d96`

Tuarā names, routes, design tokens, mandatory subtitles, and universal toast
placement are removed. The user selected the actual System Studio design tokens
and rounded controls as the baseline. The resources come from
`gethamster/oss-system`, revision `491ebdc11fe5f8270049474f7aeff86849acdc83`, under
`apps/studio/web`. [The manifest](../assets/studio/source-manifest.json) records
source paths, hashes, and the exact CSS extraction.

The snapshot records source design, not live deployment verification. It is not
a new component library and does not install dependencies. Consumer changes must
use their own supported component and build integration. Updates to this baseline
need explicit source review and corresponding provenance changes; they do not
automatically modify the source Studio application.
