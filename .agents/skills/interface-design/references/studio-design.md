# Studio design baseline

The bundled resources come from `oss-system/apps/studio/web`, not the older
standalone Studio repository. The [source manifest](../assets/studio/source-manifest.json)
records the exact revision and file hashes. Use the target application’s current
components when available; these resources make the selected baseline portable.

## Tokens and themes

[Tokens](../assets/studio/src/tokens.css) contains the exact `@theme inline`,
`:root`, and `.dark` blocks from the recorded stylesheet. It excludes application
layout, global base rules, and imports. The `@theme` mappings require the target’s
Tailwind-compatible build; the resource does not install fonts or dependencies.
The source also defines a class-based dark variant outside those token blocks:

```css
@custom-variant dark (&:is(.dark *));
```

Retain that directive or the target’s equivalent class-based configuration when
adapting the copied components. Otherwise Tailwind’s `dark:*` utilities may follow
the system color preference while `.dark` selects different token values. The
token extract alone does not configure this variant.

| Role | Studio baseline |
| --- | --- |
| Light page | `--background: oklch(0.985 0.001 106.423)`; warm stone-50. |
| Light text | `--foreground: oklch(0.147 0.004 49.25)`; stone-950. |
| Light primary action | `--primary: oklch(0.268 0.007 34.298)` with a light foreground. |
| Light soft fill | `--secondary: oklch(0.9583 0.0013 77.57)`; muted and accent alias this fill. |
| Light border | `--border: oklch(0.923 0.003 48.717)`. |
| Dark page | `--background` aliases `--neutral-990: oklch(0.191 0 0)`. |
| Dark card and popover | `--neutral-980: oklch(0.205 0 0)`. |
| Dark border | `--neutral-970: oklch(0.248 0 0)`. |
| Dark soft fill | `--secondary` aliases `--neutral-800: oklch(0.27 0 0)`; dark accent remains distinct. |
| Semantic surfaces | `--surface-canvas`, `--surface-navigation`, `--surface-page`, `--surface-panel`, and `--surface-control`. |
| Interaction surfaces | `--surface-hover`, `--surface-selected`, and `--control-hover`; primary hover fills remain opaque. |
| Semantic text and strokes | `--text-primary`, `--text-secondary`, `--text-muted`, and `--stroke-*`. |
| Base radius | `--radius: 0.65rem`; the mapped radius scale is separate from fully rounded controls. |

Retain aliases instead of pasting resolved values into individual screens. Status
colors use semantic destructive, success, warning, and information tokens; do
not use color alone to communicate a state. Read the CSS for the complete palette
and theme-specific values.

## Shape, size, and typography

[Button](../assets/studio/src/components/ui/button.tsx) uses `rounded-full`, with
default, outline, secondary, ghost, destructive, and link variants. Its default
height is `h-8`; its small and icon variants keep the same rounded treatment.
[Compact controls](../assets/studio/src/components/compact-controls.css) specify
32px height, 12px text, weight 500, 18px line-height, and a 999px radius.

[Badge](../assets/studio/src/components/ui/badge.tsx) uses a pill shape and `h-5`.
[Input](../assets/studio/src/components/ui/input.tsx) uses `h-8` and `rounded-lg`.
[FieldShell](../assets/studio/src/components/FieldShell.tsx) and its
[styles](../assets/studio/src/components/field-shell.css) own the rounded outer
field and embedded control spacing; do not independently round or restyle its
inner elements. [PillPicker](../assets/studio/src/components/PillPicker.tsx) and
its [styles](../assets/studio/src/components/pill-picker.css) preserve the same
compact rounded interaction.

The theme declares `Geist Variable` as `--font-sans`. The source’s later body rule
uses a system sans-serif stack at 14px, so do not claim every element currently
uses Geist. Preserve the target’s actual font cascade when integrating tokens.
[PageHeader styles](../assets/studio/src/components/page-header.css) use 20px/28px,
weight 600 headings, with optional 13px supporting text. They do not require a
subtitle or a count on every screen.

## Reuse and dependencies

[PanelAddAction](../assets/studio/src/components/PanelAddAction.tsx) combines the
existing button, add/cancel state, placement, and labels. Use it as an example of
reusing a full interaction rather than only a primitive.

The recorded [component configuration](../assets/studio/components.json) uses
`base-nova`, CSS variables, and Lucide icons. The source wrappers use React,
Base UI, class-variance-authority, `cn`, Tailwind utilities, and target aliases.
The asset directory preserves relative files but is not a standalone installed
package. Adapt through the consuming repository’s supported dependencies and
aliases; do not install a competing stack solely to match a reference.

The [shadcn notice](../assets/studio/src/styles/shadcn-LICENSE.md) accompanies the
upstream-derived component references. Keep it when redistributing those sources.
