# AX base-template patch

The AX controller at the pinned revision ignores the selected base template when
creating templates for custom runner images. This patch copies the base guest's
`SecurityContext` and its gVisor `SandboxConfig` into newly derived templates.
A template preparation error fails the task instead of launching a fallback.

The patch adds no capabilities by itself. Configure the base template's guest
capabilities and select a separate gVisor `SandboxConfig` before creating a task.
AX still supplies the runner image, environment, workspace and snapshot settings.
Missing base settings retain AX defaults; a non-gVisor sandbox class is rejected.
Existing derived templates remain unchanged, so base changes require a new Horde
runtime identity.

[manifest.json](manifest.json) records the upstream commit and patch SHA-256.
The patch retains the upstream [Apache-2.0 license](../../../LICENSE).

To reproduce the patched source, start with a clean checkout of
[Google AX](https://github.com/google/ax) at the recorded commit. Verify the patch
checksum against the manifest, then run these commands from the AX checkout:

```sh
AX_PATCH=/path/to/horde/containers/ax/patches/base-template.patch
test "$(git rev-parse HEAD)" = d8ed0fe38bceb7842d3c47817d53d16ccdfcb601
git apply --check "$AX_PATCH"
git apply "$AX_PATCH"
go test ./...
```

Build `ax-controller` from that source and pin the resulting container image by
digest. Horde's `ax_revision` remains the upstream base commit; record the patch
checksum with the controller image digest in deployment records.

The patch includes seven regression tests covering inheritance, independent
copies, unchanged defaults and failure without fallback. AX acknowledges failed
reconciliation events, so correcting a transient preparation error requires a new
reconciliation trigger. The patch does not add automatic retries or replay work.
