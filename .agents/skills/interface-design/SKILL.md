---
name: interface-design
description: >-
  Use when designing, implementing, or reviewing product interfaces, including
  prototypes. Reuse complete interactions and the Studio design baseline, with
  semantic tokens, rounded controls, useful content, and accessible behavior.
metadata:
  audience: "contributors"
---

# Interface Design

Start with the user’s task and the existing interface. Every visible control and
sentence should support an action, decision, or needed recovery. Preserve
explicit user constraints and the target repository’s established design contract.
This skill guides interface contributors; it is not a runtime agent persona.

## Use the design baseline

Read [Studio design](references/studio-design.md) for the token roles, typography,
rounded controls, and bundled component references. Use this baseline for System
interfaces and new shared OSS surfaces unless the target has an explicit different
design contract. Extend an existing product consistently rather than silently
replacing its design system or installing a second component library.

Use semantic color and surface tokens rather than screen-specific hardcoded
colors. Preserve light and dark theme relationships. Buttons, badges, pickers,
and compact controls use the established pill treatment; do not apply full
rounding indiscriminately to panels, inputs, or content surfaces.

## Reuse complete interactions

Inspect equivalent screens and use the component that owns behavior, placement,
spacing, icons, dimensions, states, and accessibility. A shared button inside
several bespoke toolbars does not provide a shared interaction. Add a necessary
variant to the existing component instead of creating another version.

The bundled Studio controls are source references with recorded provenance.
Inspect the live target’s corresponding components and dependencies before
adapting them; a copied reference is not proof of a working integration. Preserve
the original license notice when reusing upstream-derived component code.

## Keep the surface focused

Omit redundant counts, decorative labels, repeated branding, and explanatory
footers that do not help the user act. Do not replace removed filler with a
badge, tooltip, or helper sentence that says the same thing. Avoid invented
navigation and actions outside the requested workflow.

Headers and feedback should fit the operation. Do not impose a subtitle on every
screen or route every error to a toast. Use
[Feedback and accessibility](references/feedback-and-accessibility.md) for
validation, recovery, keyboard behavior, and state coverage. Describe prototype
limits in delivery notes unless a particular action needs an in-context
explanation to avoid misleading the user.

## Verify the actual interface

Choose states and viewports affected by the change and reuse valid observations.
A copy or spacing fix does not require a full state/theme/device matrix or a new
UI test suite. Stop when the affected behavior and required checks pass.

Inspect the rendered result in relevant populated, empty, loading, failure,
selected, disabled, and keyboard-focus states. Check the applicable themes and
narrow layouts, and compare the changed interaction with a sibling screen.
Preserve meaningful labels, contrast, focus visibility, and recovery behavior.

Run the target’s required checks and verify the affected user operation. Report
which views and states were actually observed; static resources alone do not
establish accessibility or runtime behavior. Follow
[verified-delivery](../verified-delivery/SKILL.md) for scope and delivery evidence.

The [source record](references/source.md) identifies the adapted guidance and
Studio snapshot; it does not authorize changes to the source application.
