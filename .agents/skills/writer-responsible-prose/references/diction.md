# Diction patterns

Words and phrases that mark text as generated regardless of structure. One of these alone proves nothing, so judge in clusters and check [guardrails.md](guardrails.md) before flagging someone else's writing. Preserve established technical senses (*robust* statistics, *leverage* in finance, *key* in cryptography).

## 1. Prestige vocabulary

Watch: actually, additionally, align with, boasts, breathtaking, commitment to, crucial, delve, elevate, emphasizing, enduring, enhance, ever-evolving, foster, furthermore, game-changer, garner, groundbreaking (figurative), holistic, in the heart of, interplay, intricate, journey (figurative), key (adjective), landscape (abstract), leverage (verb), moreover, multifaceted, must-visit, nestled, pivotal, profound, quietly, realm, renowned, rich (figurative), robust, seamless, showcase, stands as, streamline, stunning, tapestry (abstract), testament, underscore (verb), vibrant.

Replace with plain verbs and nouns: *is*, *has*, *also*, *use*, *helps*.

Bad: `Additionally, an enduring testament to Italian colonial influence is the widespread adoption of pasta in the local culinary landscape, showcasing how these dishes have integrated into the traditional diet.`

Good: `Pasta dishes, introduced during Italian colonization, remain common, especially in the south.`

Test: two watch words in one paragraph means rewrite the paragraph, since these words travel in packs.

## 2. Fancy copulas

Watch: serves as, stands as, functions as, acts as, represents, marks, boasts, features, offers.

Reaching past *is* and *has* is a tell on its own.

Bad: `Gallery 825 serves as LAAA's exhibition space for contemporary art. The gallery features four separate spaces and boasts over 3,000 square feet.`

Good: `Gallery 825 is LAAA's exhibition space for contemporary art. The gallery has four rooms totaling 3,000 square feet.`

## 3. Inflated importance

Watch: a pivotal moment, marking a shift, key turning point, evolving landscape, focal point, indelible mark, deeply rooted, setting the stage for, reflects broader, underscores its significance, part of a broader movement.

Generated prose claims that ordinary details mark major change or prove a legacy. Keep the fact, drop the trend.

Bad: `The institute was officially established in 1989, marking a pivotal moment in the evolution of regional statistics.`

Good: `The institute was established in 1989, part of a wider decentralization of administrative functions.`

## 4. Sales language

Watch: vibrant, nestled, breathtaking, stunning, renowned, must-visit, rich cultural heritage, natural beauty, exemplifies, commitment to excellence.

Bad: `Nestled within the breathtaking region of Gonder, Alamata stands as a vibrant town with a rich cultural heritage.`

Good: `Alamata is a town in the Gonder region of Ethiopia.`

## 5. Shallow -ing analysis

Watch: highlighting…, underscoring…, ensuring…, reflecting…, symbolizing…, showcasing…, fostering…, contributing to…, encompassing….

The trailing participial clause claims meaning the sentence never established. Cut the clause, or promote the claim to its own supported sentence.

Bad: `The temple's colors resonate with the region's natural beauty, reflecting the community's deep connection to the land.`

Good: `The temple is painted blue, green, and gold, colors meant to evoke bluebonnets and the Gulf.`

## 6. Vague sources and name-dropping

Watch: experts believe, industry reports, observers have cited, some critics argue, many teams find, and lists of famous outlets or follower counts standing in for substance.

Every appeal to authority names its source or dies. Never invent a source to fill the hole.

Bad: `Experts believe it plays a crucial role in the regional ecosystem.`

Good: `Researchers at the provincial conservation office study the river's role in the ecosystem.` (Only if the source says so; otherwise cut the claim.)

## 7. Qualifier stacking

Watch: could potentially, might arguably, it's also possible, in some cases it may, to be fair.

One qualifier per claim, kept only when the meaning needs it. Remove caveats that exist to repair an earlier overstatement.

Bad: `It could potentially be argued that the policy might have some effect on outcomes.`

Good: `The policy may affect outcomes.`

## 8. Filler frames

- `in order to achieve this goal` → `to achieve this`
- `due to the fact that` → `because`
- `at this point in time` → `now`
- `in the event that` → `if`
- `has the ability to` → `can`
- `it is important to note that` → say the thing

## 9. False ranges

Template: `from X to Y` where X and Y are not endpoints of anything.

Bad: `The book takes us from the singularity of the Big Bang to the enigmatic dance of dark matter.`

Good: `The book covers the Big Bang, star formation, and current theories about dark matter.`

Test: swap X and Y. If the sentence means the same thing, it was a list in a costume, so list the items.

## 10. Fake alternatives and unraised objections

Watch: a tempting approach would be, one might be tempted to, you might think… but, some would suggest; and on the defensive side: this isn't mainly about, I'm not saying, to be clear, don't get me wrong, some might say… but.

Generated prose invents an option nobody would take, rejects it in a clause, and moves on, or answers an objection that appears nowhere in the text. Remove the fake option and state the real constraint. Keep an objection when the text names who holds it or answers it in full, and keep real alternatives a reader would weigh in a design doc or tutorial.

Bad: `A tempting approach would be to rotate tokens by restarting the auth service, but that would drop every session. Rotation happens in place.`

Good: `Tokens rotate in place every 24 hours, and clients refresh transparently.`

## 11. Formulaic sayings and fake depth

Watch: X is the Y of Z, the currency of, the language of, the architecture of, becomes a trap; the real question is, at its core, what really matters, the heart of the matter, fundamentally.

The saying sounds deep while adding no detail, so replace it with the specific claim.

Bad: `Symmetry is the language of trust.`

Good: `Symmetric layouts feel more predictable to users.`

## 12. Fake-candid openers

Watch: Honestly?, Look,, Here's the thing,, Let's be honest,, Real talk, as standalone hooks before an ordinary point.

State the point directly. Mid-sentence *honestly* is ordinary speech and stays.

Bad: `Is it worth the price? Honestly? It depends on how often you'll use it.`

Good: `Whether it's worth the price depends on how often you'll use it.`

## 13. Hyphenated pair pileups

Watch: cross-functional, data-driven, client-facing, end-to-end, high-quality stacked in one noun phrase.

Keep the hyphen grammar requires before a noun (`a high-quality report`), drop it after (`the report is high quality`), and never chain three modifier pairs in a row.
