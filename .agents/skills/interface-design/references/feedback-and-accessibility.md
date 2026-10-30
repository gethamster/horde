# Feedback and accessibility

Choose feedback according to the user’s task and the established interaction.
A brief notification can confirm a completed action; a field error belongs near
the field the user can correct. A persistent failure may need persistent recovery
controls. Do not make one feedback channel mandatory for every operation.

The [Studio feedback source](../assets/studio/src/components/ActionFeedback.tsx)
and its [styles](../assets/studio/src/components/action-feedback.css) provide
an existing shared implementation with a top-center viewport, rounded surface,
and reduced-motion behavior. Reuse the target’s owner rather than creating a
second toast system or copying a location without considering the task.

## Preserve useful states

Use the checks below for states and interactions affected by the change. They are
not a complete matrix to execute for every UI edit. Reuse valid evidence and keep
new permanent tests only when their ongoing value justifies them.

Check relevant loading, empty, populated, disabled, selected, expanded, success,
and failure states. Preserve recoverable user input and make the next valid
action clear. Do not substitute a misleading success message for a failed request.
Expose request or operation references only where they help recovery and are safe
for the current audience; avoid printing private content or credentials.

Keep headers concise and semantic. Use the existing
[PageHeader](../assets/studio/src/components/PageHeader.tsx) for equivalent views.
Its optional count and subtitle are capabilities, not instructions to fill them.
Only include them when they answer a real question or affect a decision.

## Verify interaction, not only appearance

Use semantic controls, programmatic labels, correct disabled and expanded states,
and visible keyboard focus. Icon-only controls still need an accessible name.
Check focus order, popup dismissal, restored focus, readable errors, and relevant
assistive-technology behavior when changing an interaction.

The recorded global stylesheet uses hover backgrounds, opacity, and border
changes for keyboard focus rather than an outer ring. The token extract omits
those global rules. Importing the token file alone does not provide focus styling;
preserve or implement visible focus in the consuming interface and verify it in
both themes. Never remove focus visibility just to match a screenshot.

Check narrow layouts and zoom so controls remain reachable and content does not
clip. Respect reduced-motion preferences for animations. Compare corresponding
controls across sibling views for consistent hit areas, icons, spacing, and states.
Static source snapshots document intent; validate actual rendered behavior after
integration without claiming that a resource import itself is a UI test.
