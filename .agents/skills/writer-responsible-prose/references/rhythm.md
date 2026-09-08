# Rhythm patterns

Every sentence here can pass the word rules while the passage still reads as generated, because these patterns live across sentences. Labels like `Bad:` / `Good:` / `Test:` are document machinery in this file, allowed everywhere in the skill.

The default register for explanatory prose is flat explanation: sentences that carry information and end when the information ends. Emphasis is a budget, and a longform piece gets at most two or three engineered moments, a short post at most one.

## 1. Forced triplets and anaphora

Template: three parallel clauses or sentences sharing a frame. `Wikis tried organization. Docs-as-code tried proximity. Living documentation tried culture.` Also the noun triple (`innovation, inspiration, and industry insights`) and the escalation ladder (`the cheap version… the expensive version… the worst version`), which is the triplet wearing a costume.

Models force ideas into threes because the shape scores as complete. Collapse into one loose sentence listing actual behaviors, or keep two legs and cut the filler leg.

Bad: `The event features keynote sessions, panel discussions, and networking opportunities. Attendees can expect innovation, inspiration, and industry insights.`

Good: `The event includes talks and panels, with time for informal networking between sessions.`

Test: delete or reorder one leg. If nothing is lost, the triple was rhythm, so break it. A repeated frame earns its place only when the legs are real items whose order carries information, or when the repetition reads aloud as speech rather than layout.

## 2. Contrast scaffolding

Template: `not X but Y`, `It's not just X, it's Y`, `X used to be A. Now it's B.`, `instead of [bad thing], [good thing]`, and paired short sentences used as a running beat (`X flags. Y fixes.`).

One contrast per piece is fine when the contrast is the point. As a recurring rhythm it is scaffolding, and the fix is to state the fact plainly and let the reader do the math.

Bad: `It's not just about the beat riding under the vocals; it's part of the aggression and atmosphere. It's not merely a song, it's a statement.`

Good: `The heavy beat adds to the aggressive tone.`

Test: count the contrast shapes. Keep the one carrying the argument and rewrite the rest as plain statements.

## 3. Punchline stacking

Template: a row of dramatic fragments, or an aphorism closing every paragraph, so the piece reads like a highlight reel with no game.

Bad: `Then AlphaEvolve arrived. It had no preference for symmetry. No aesthetic prior. No nostalgia for human taste. The old rules were gone.`

Good: `AlphaEvolve changed the search because it did not favor symmetry or human-looking designs, which made some of the older assumptions less useful.`

One short sentence for emphasis is fine. Several in a row is percussion, and an engineered antithesis (`prose reads fine and executes terribly`) is a punchline even at normal length.

Test: count the lines written to be quoted rather than read. Past the budget above, rewrite the extras as flat explanation.

## 4. Colon-punch labels

Template: `one goal: knowledge that stays true.` / `the key constraint:` / `the line that stings:` followed by the payoff. Setup, colon, punch is a performance structure.

Cut the label and write the sentence, or cut the sentence. A colon that introduces a real list or a worked example is doing grammar, which stays.

Test: read the words before the colon alone. If they only announce that a point is coming, delete them.

## 5. Staged vignettes

Template: an invented second-person scene built from short declaratives toward a reveal. `You write a blueprint. It's accurate on day one. Three months later the code moved…` Scene furniture (`the status flips green`) and invented dialogue count too.

Readers now pattern-match this instantly because the details were never witnessed. Real anecdotes with real, checkable details stay. Invented scenes become one direct sentence, and one real example beats three staged ones.

Bad: `You write a blueprint. It's accurate on day one. Three months later the code moved and the blueprint still says the old thing.`

Good: `A blueprint is accurate the day it's written. The code moves, decisions change underneath it, and the doc keeps saying the old thing.`

Test: second person, hypothetical, building to a reveal, with details nobody saw. All four present means cut.

## 6. Synonym cycling and repeated openings

Template: renaming one referent to avoid repetition (`the protagonist… the main character… the central figure… the hero`), or opening several sentences identically (`It checks… It reads… It posts…`).

Pick one name per referent and hold it for the piece. For repeated openings, merge sentences or lead with the action. Fix the repeated pattern, never the word itself; the surviving sentence may still start with `She`.

Bad: `She noted the door. She noted the lock on it. She filed both away.`

Good: `She noted the door and its lock, then filed both away.`

Test: two names for one thing in one passage, or three identical openings in a row, means rewrite.

## 7. Meta copy

Template: sentences about the text instead of the subject. `Let's dive in`, `here's what you need to know`, `the rest of this guide walks each stage`, a heading restated by its first sentence, a closing paragraph that summarizes what was just said.

The text speaks to the reader about the subject: what to do, when, and what you get. Transitions happen by starting the next section, never by announcing it. Casual framing (`one thing that bit me, so pay attention`) is the same pattern in a hoodie, so remove the announcement, not just its formal tone.

Bad: `Let's dive into how caching works in Next.js. Here's what you need to know.`

Good: `Next.js caches data at multiple layers, including request memoization, the data cache, and the router cache.`

Test: delete every sentence whose subject is the article, the section, or the explanation. If the piece still works, they were meta.

## 8. Padding shapes

Sentence tails that restate instead of add:

- Trailing because-clauses that repeat the consequence in fancier clothes. `it never guesses, because a guessed update is drift wearing a fresh timestamp` becomes `it never guesses.`
- Summary echoes, where a paragraph's last sentence restates the paragraph. Cut it and check whether anything was lost. Usually nothing was.
- Redundant scope and time phrases (`the same day it happens` after a trigger already said when).
- Speculative tails about the reader (`and it's usually more than you'd guess`).

Test: halve any sentence over about 25 words and keep the halved version unless real meaning disappeared.

## 9. Even cadence

Generated prose drifts toward uniform mid-length sentences. Real writing alternates short and long. After a rewrite, read the passage aloud, and if every sentence lands at the same length, merge two and split one.
