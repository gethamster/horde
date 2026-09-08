---
name: writer-responsible-prose
description: Write and edit English so it reads as deliberate American writer-responsible prose with no generated tells at any level. Converts Hindi/Indo-Aryan topic-comment cadence and LLM caption sentences into American information structure, and subsumes the humanizer skill (Wikipedia's "Signs of AI writing") end to end, covering rhythm-level slop (forced triplets, contrast scaffolding, punchline stacking, staged vignettes, meta copy), word-level tells (delve, robust, testament, shallow -ing analysis, qualifier stacking, vague sources), and formatting or chatbot artifacts (em dashes, bold overuse, emojis, "I hope this helps"). Use when drafting, editing, rewriting, reviewing, or humanizing prose, bullets, emails, memos, docs, product copy, or posts, and when the user says humanize, de-slop, AI-sounding, sounds like ChatGPT, remove AI tells, make it sound human or American, Indian English, topic-comment, staccato, captions after a period, or anti-slop.
license: MIT
metadata:
  version: "2.0"
  type: writing
---

# Writer-responsible prose

American explanatory English names its noun and subordinates the label. Hindi and other Indo-Aryan languages hang given information out front as a topic, drop recoverable nouns, and put the comment in the next clause. Indian English inherited that information structure, and LLMs independently learned the same move because "declare, then relabel" looks like clarity to a reward model, so the two together produce a cadence Americans hear as official, academic, and off.

The same reward pressure leaves fingerprints above and below the sentence. Above it: forced triplets, contrast scaffolding, a punchline in every paragraph, staged scenes, copy that narrates the piece instead of the subject. Below it: a prestige vocabulary (*delve*, *testament*, *landscape*), copulas dressed up as *serves as*, analysis smuggled in through *-ing* clauses, qualifier stacks, chatbot residue. This skill edits all three levels, converting into American written register without treating Indian English as broken and without changing what the text says.

These files must follow their own rules. Caption sentences, clefts, triplets, colon-punch labels, and em dashes are banned here too, except inside `Bad:` examples. Labels like `Bad:` / `Good:` / `Test:` / `Watch:` are document machinery, allowed.

## Reading map

Work the levels in order, structure first, because a structural rewrite usually deletes half the rhythm and diction problems on its own.

1. [references/why.md](references/why.md) explains the substrate. Read it once if the diagnosis is unclear.
2. [references/patterns.md](references/patterns.md) covers clause and sentence structure: captions, clefts, headless topics, staccato, passives.
3. [references/rhythm.md](references/rhythm.md) covers across-sentence tells: triplets, contrast scaffolding, punchline stacking, vignettes, meta copy, padding.
4. [references/diction.md](references/diction.md) covers words and phrases: the vocabulary watch lists, fancy copulas, -ing analysis, vague sources, filler.
5. [references/artifacts.md](references/artifacts.md) covers formatting and chatbot residue: dashes, bold, emojis, leftovers, stock sections.
6. [references/guardrails.md](references/guardrails.md) covers what to keep: the rewrite contract, voice matching, false positives. Read it before rewriting someone else's text.
7. [references/office-lexicon.md](references/office-lexicon.md) covers Indian-office phrasing and grammar leftovers.

## Do

1. Keep every claim and never invent a fact, name, number, date, quote, or citation. The full contract is in guardrails.md.
2. Fold the label into the fact with *which*, *so*, *and*, or an appositive. One sentence that does two jobs beats two sentences where the second names the first.
3. Repeat the noun, and hold one name per referent for the whole piece. If a bullet cannot be pasted into a blank message and still parse, the noun is missing.
4. Put consequences in a subordinate clause hanging off the fact, not in a new sentence whose subject is a noun you just minted.
5. Give sentences concrete subjects (people, buyers, GPUs, hours, prices) rather than *the winning business case* or *smoother utilization*, and prefer *is* and *has* over *serves as* and *boasts*.
6. Prefer *because* / *so* / *which* over *is what* / *is how* / *is where*.
7. Write flat explanation by default: sentences that carry information and end when it ends. Budget emphasis at two or three engineered moments in longform, at most one in a post.
8. Match the writer's sample when one exists. The sample overrides these rules wherever they conflict.

## Do not

Ban these templates on sight and rewrite them rather than polishing them.

| Ban | Why it jars | Fix |
|---|---|---|
| `X. That is Y.` / `X. This is Y.` | Caption after a period, a reversed pseudo-cleft used as a reformulation. | Merge to `X, which is Y` / `X, Y` / `X, so Y`. |
| `X is what Y` / `X is how Y` / `That's what Y` | Specificational cleft that pretends to name an essence and usually adds nothing. | `X that Y` or `Y because X`. |
| Three short `NP is NP` sentences in a row | Parataxis / staccato burst, the LLM default punch. | Join two of them with *and*, *so*, or a semicolon. |
| Bare topic as subject (`Spare moves…`) | Unlinked topic plus pro-drop, which leaves the reader to supply the noun. | `Spare capacity moves…` |
| `The [abstract noun] is [definition].` | Copular equative with a prestige subject. | State the claim and drop the frame. |
| Next sentence reuses the last one's abstraction as its subject | Relay race of abstractions. | Use *which* / *so*, or keep a concrete subject. |
| Three parallel clauses sharing a frame | Forced triplet, the shape that scores as complete. | Collapse to one loose sentence or cut a leg. |
| `not X but Y` / `It's not just X, it's Y` / `X used to be A. Now it's B.` | Contrast scaffolding as a running beat. | Keep at most one contrast per piece, the one carrying the argument. |
| A row of dramatic fragments or an aphorism per paragraph | Punchline stacking. | Rewrite the extras as flat explanation. |
| `one goal: X` / `the key constraint:` | Colon-punch label, setup and payoff as performance. | Cut the label and write the sentence. |
| Em or en dash anywhere | Flagged generation tell. | Comma, period, colon, parentheses, or rewrite. Sample overrides. |
| `Let's dive in` / heading restated by its first sentence | Meta copy about the text instead of the subject. | Delete and start at the first fact. |

The full pattern sets with worked pairs live in rhythm.md, diction.md, and artifacts.md. Also drop Indian-office leftovers: never write *kindly*, *revert* (for reply), *do the needful*, *the same*, *prepone*, *out of station*, *humble request*, *for your kind perusal*.

## Tiny examples

Bad: `In 75% of hours, at least 6% of peak tok/s is available to sell. That is incremental revenue on GPUs you already own.`

Good: `In 75% of hours you can sell at least 6% of peak tok/s, extra revenue from GPUs you already own.`

Bad: `A real market is what lets quiet hours clear.`

Good: `Quiet hours clear when a real market can match them to demand.`

Bad: `Additionally, the platform boasts a robust suite of tools, showcasing its commitment to seamless collaboration.`

Good: `The platform also includes collaboration tools.`

Bad: `Wikis tried organization. Docs-as-code tried proximity. Living documentation tried culture.`

Good: `Teams have kept docs next to the code, agreed that whoever ships updates the doc, and run cleanup passes, and each habit decayed on its own.`

More pairs in every reference file.

## Preflight

Run three scans before sending any draft, then the fact check.

Structure scan:

- Any sentence starting with *That is* / *This is* / *That's what*? Merge it.
- Any *is what* / *is how* / *is where* / *is why*? Kill the cleft.
- Three short equatives in a row? Join two.
- Can each bullet stand alone? If a stranger would ask "spare what?", restore the noun.
- Is the subject an abstract noun you just coined? Recast with a concrete subject.
- Does any sentence exist only to say what the last sentence was? Cut or merge it.

Rhythm scan:

- Any three-leg parallel structure, including escalation ladders? Break it.
- More than one contrast shape? Keep the load-bearing one.
- Count the engineered moments against the budget in Do 7.
- Any second-person scene nobody witnessed? One direct sentence, or one real example.
- Any sentence about the piece instead of the subject? Delete it.
- Two names for one referent, or three sentences opening identically? Fix the pattern.
- Any sentence over ~25 words? Halve it and keep the halved version unless meaning was lost.

Diction and artifact scan:

- Search for `—` and `–`. Remove each unless the writer's sample earns it.
- Any watch-list word, *serves as*, or a trailing *-ing* clause claiming significance? Swap for the plain form.
- More than one qualifier on a claim, or an unnamed expert? Trim or name the source.
- Any *kindly*, *revert*, *the same*, *do the needful*? Replace.
- Chat residue, emojis, decorative bold, title-case headings, a generic upbeat ending? Delete, and end on the last concrete fact.

Fact check: did the rewrite add or drop any claim, name, number, date, or quote? Either one is an error, so fix it before returning anything. Then read the result aloud, and if every sentence lands at the same length, merge two and split one.

## Returning results

For pasted text, return the rewrite plus a short note on any patterns you left in place and why. When editing a file, change prose only, keep code blocks, YAML, data, and link targets untouched, and summarize what changed. When another task embeds this skill, return only the final text.

## Growing this skill

This skill improves by autopsy. When a reader flags a sentence as off, or an edit catches a tell no rule here names:

1. Extract the template, the way patterns.md does, rather than banning the specific words.
2. Add it to the file that owns its level (structure, rhythm, diction, artifacts) with one real `Bad:` / `Good:` pair and a one-line `Test:`.
3. If the new rule could flag legitimate human writing, add the counter-case to guardrails.md in the same change.
4. Bump the minor version in this frontmatter and add a line to [CHANGELOG.md](CHANGELOG.md).

A rule nobody can test gets cut, and every addition must pass the preflight above.
