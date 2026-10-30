---
name: rust-review
description: >-
  Review existing Rust code for idiom, API design, error handling, safety, and
  maintainability, and report actionable findings with concrete consequences and fixes. Use this whenever the user asks to review, audit,
  critique, grade, or "check" Rust code, a crate, a module, a .rs file, a diff, or a pull
  request, or asks what is wrong with their Rust or how to make it more idiomatic, even if
  they don't say "review". Pairs with the rust-best-practices skill, which covers writing
  new code.
---

# Rust review

Review Rust code against the same rules the `rust-best-practices` skill uses for writing
it, and report what to change. The output is a review, not a rewrite: the reader decides
what to fix. Only rewrite code when the user asks for that.

Use this skill for a requested review or an actual repository review requirement;
it does not add a review stage to every Rust fix. Follow
[verified-delivery](../verified-delivery/SKILL.md) for proportional effort and the
stopping condition. Focus on actionable consequences within the requested scope.

## 1. Decide the scope

- **What to read.** Review exactly what the user pointed at: a file, a module, a crate,
  or a diff. For a diff, review the changed lines and anything they break, not the whole
  file.
- **Library or application.** Check for a `lib.rs`, `pub` items used by other crates, or
  a published `Cargo.toml`. Library code is held to the API-design rules strictly.
  Application code gets the simplicity rules instead: concrete types, `anyhow`-style
  errors, and no speculative generality. Say which you assumed at the top of the report.
- **The edition and MSRV**, from `Cargo.toml`, so you don't recommend features the
  project can't use.
- **Frameworks.** If the crate depends on `bevy` or `bevy_ecs`, read the Bevy digest
  (`bevy.md`) in the `rust-best-practices` skill's references first. It lists the Bevy
  conventions to leave alone (by-value system parameters, the prelude, plain-data
  components) and the ECS bugs to look for.

## 2. Use available evidence

Read existing check results before running tools. Run a targeted check only when
it resolves a concrete uncertainty or satisfies an actual repository requirement.
Do not automatically rerun passing tests or add all-targets, all-features,
pedantic Clippy, benchmarks, or documentation builds to a review.

Summarize relevant results and distinguish observed failures from unverified
concerns. If execution is unavailable, review the source and state the limit.

## 3. What to look for, in priority order

Prioritize correctness and soundness. Consult the other categories only as they
apply to the requested scope; this is not a quota or a mandate to redesign APIs.

**Correctness and soundness**

- Undefined behavior: an `unsafe` block without a `// SAFETY:` comment, `transmute` to
  cast enums or extend lifetimes, `unsafe impl Send`/`Sync` used to silence the compiler,
  or a safe function that can cause UB through safe calls (unsound).
- Panics on reachable paths: `unwrap()`, `expect()`, indexing, `as` casts that wrap, or
  integer overflow, wherever input from outside can reach them.
- Trait contracts: a hand-written `Ord` with a derived `PartialEq`, or `Hash` that
  disagrees with `Eq`. These silently corrupt `BTreeMap`, `HashMap`, `sort`, and `dedup`.
- Lock hazards: a `MutexGuard` held across `.await` or returned to a caller, user
  closures called under a lock, or a poisoned lock recovered with `into_inner`.
- ECS bugs in Bevy code: two queries that can reach the same component mutably (a
  B0001 panic on the first frame), systems that read each other's results with no
  order between them, and deferred `Commands` read back before they apply.
- Destructors that panic, block, or can fail.

**Error handling**

- Errors that callers can't use: `()`, `String`, or `Box<dyn Error>` as a library's
  public error type, an error enum without `Display` and `std::error::Error`, or
  `anyhow::Error` in a library's public API.
- Errors that lose information: `map_err(|_| ...)`, no `source()`, or messages without
  the path, line, or value that failed.
- Panics used for expected failures, or `catch_unwind` used as control flow.

**API design** (when it affects actual callers or the requested API review)

- Parameters: `&String`, `&Vec<T>`, or `&Box<T>` instead of `&str`, `&[T]`, or `&T`; a
  boolean whose meaning is unclear at actual call sites; ownership taken and then cloned, or a borrow
  cloned internally to get ownership.
- Visibility: public fields that should be private, `Rc`, `RefCell`, or `Arc` in public
  signatures, or third-party types leaking into the API.
- Missing standard traits: `Debug` on public types, `Display` on errors, `Default` next to
  a zero-argument `new()`, and `FromStr` instead of an ad-hoc `from_str` method.
- Naming: `get_` prefixes on getters, `as_`/`to_`/`into_` that don't match their cost and
  ownership, and `Manager`/`Service`/`Util` names.
- Evolution: public enums or structs that should be `#[non_exhaustive]`, or traits that
  should be sealed.

**Idiom and clarity**

- Clones that exist only to satisfy the borrow checker.
- Check-then-unwrap (`if x.is_some() { x.unwrap() }`), and `match` where a combinator is
  clearer, or the reverse.
- String building with `+`, hand loops that are clearer as iterators, or iterator chains
  long enough to need a loop.
- `HashMap` iteration order leaking into output or tests.
- `#![deny(warnings)]` in source, `#[allow]` without a reason, and glob imports of
  external crates (a framework's documented prelude, such as `bevy::prelude`, is fine).

**Documentation and tests**

- Public items without docs, and docs that restate the name.
- Missing `# Errors`, `# Panics`, or `# Safety` sections where they apply.
- Doc examples that use `unwrap()` instead of `?`.
- Missing evidence for a concrete behavior at risk, or tests that only restate
  constants. The absence of a test file alone is not a finding.

The `rust-best-practices` skill and its `references/` folder hold the full rules and their
reasoning. If it is installed, read the relevant reference when a finding needs a rule
you're unsure of. Otherwise, rely on the summary above.

## 4. Write the report

Lead with actionable findings, ordered by impact. Give each finding a location,
concrete consequence, and the smallest useful fix. Cite a technical rule when it
helps substantiate the issue; use a short code example only when clearer than prose.
Group repeated instances. Separate verified defects from uncertain concerns.

- **Must fix:** a demonstrated bug, soundness problem, or violation of an actual
  requirement or compatibility contract.
- **Should fix:** a supported cost to current callers or maintainers within scope.
- **Optional:** style or potential improvements, only when the requested review
  includes them. They do not block an otherwise valid fix.

Do not invent findings, require a minimum count, or pad the report with praise.
If there are no actionable findings, say so and give the verification limits.
Respect deliberate behavior and focus on concrete consequences, not hypothetical
future extensibility. Report once; another review needs changed code, new evidence,
or an explicit request.

## 5. Lint configuration is separate work

Recommend or change lint configuration only when requested or when it directly
solves the demonstrated issue. A missing `[lints]` table does not automatically
justify adopting another project's lint pack or expanding CI.
