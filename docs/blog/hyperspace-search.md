---
title: "Search at Light Speed: Inside Lume's Hyperspace Vector Field"
date: 2026-06-19
tags: [search, AI, vector-search, RAG, agentic-ai, visualization, rust, three.js]
description: >
  We turned search into a living 3D vector field where results warp in from
  hyperspace and an agent searches itself until it finds the cited answer.
  Here's how it works — and the one-line bug that nearly hid it.
---

# Search at Light Speed: Inside Lume's Hyperspace Vector Field

I put on *Derezzed* by Daft Punk, hit search, and watched the results **warp in from hyperspace** — orbs streaking out of the void, decelerating hard, and snapping into formation like a fleet dropping out of lightspeed.

This is **Lume**. It doesn't return a static list of blue links. It renders your query as a living 3D vector field, then runs an answering loop over the evidence it retrieves.

---

## Search You Fly Through, Not Scroll Past

Traditional search engines hand you ten blue links and wish you luck. Lume hands you a spatial **field**. 

Every result is an orb positioned dynamically in 3D space:
* **Size** encodes relevance weight.
* **Color** encodes which query it answered (Lume supports additive, multi-facet search — stack as many queries as you like).
* **Position** comes from the shared simulation space: clusters separate, related passages pull toward their query anchors, and **overlap halos** glow gold where multiple queries surface the same passage.

The layout isn't a static snapshot. The field relaxes frame by frame using a phase-binding and Weber-style simulation until the structure of the answer space becomes visible. You orbit it, zoom in, and hover over orbs to read the underlying text.

When a new query is executed, the results **jump in via hyperspace**: each orb is flung far out along its radial vector, stretched into a thin streak, and eased back into place in a staggered cascade. The motion reveals the topological shape of the result set as it forms.

---

## An Agent That Argues With Itself

Visualization is the surface. Underneath is **agentic answering**: Lume plans searches, checks whether the retrieved passages answer the question, and refines when they do not.

When you ask a question, Lume runs a loop:
1. **Plan:** Write its own keyword search queries.
2. **Retrieve:** Pull candidates into the field (which you watch warp in).
3. **Evaluate:** Judge its own evidence: *do these passages actually answer the question?*
4. **Refine:** If not, rewrite queries from a different angle and search again.
5. **Answer:** Synthesize a concise, inline-cited response, lighting up the cited orbs in the field as provenance anchors.

I tested Lume on the full text of *The Count of Monte Cristo* (1,926 passages). 

**"How does Edmond Dantès's father die?"** The answer is buried 26 chapters deep, in a scene where the father is never called "father" and his death is never called "death" — Caderousse simply recounts that the old man *"died of downright starvation."* Lume planned, retrieved, judged the evidence sufficient, and quoted it with a citation to Chapter 26 in seconds.

Then I threw a curveball: **"How does Caderousse die?"**

Its first query plan guessed *"killed by Danglars."* Finding no evidence, it ruled the retrieved passages **insufficient**, refined the query, searched again, ruled *that* insufficient, and refined a third time to find the correct answer:

> *"Caderousse dies from mortal wounds after being murdered by his comrade in the galleys at Toulon, the Corsican Benedetto (who also calls himself Andrea Cavalcanti)."* — cited to Chapter 83.

A search loop that can form a bad hypothesis, reject it against retrieved evidence, and try a different angle is materially different from a one-shot lookup.

---

## The One-Line Bug That Nearly Hid All of This

Here is the honest engineering footnote: the failure was more instructive than the success.

The first time I asked about Dantès's father, Lume answered: *"The provided passages do not contain information regarding how Edmond Dantès's father died."*

But the answer was in the corpus. We traced the failure. The raw retrieval was fine; the death scene ranked high for keyword content. The problem was upstream in the **query planner**. 

Every query the planner generated restated the question's proper nouns — "Dantès," "father," "death." In a richly indexed corpus, those tokens had too much pull: the literal chapter title *"Father and Son"* and broad entity matches pushed the top evidence toward passages about the father while alive. The agent was fed the wrong slice of the corpus and concluded the answer was missing.

The planner couldn't bridge the vocabulary gap between the user's question and the prose's narrative of the event.

To fix it, we diversified the query planner to generate synonyms and event descriptions, scaled the LLM's feedback window with candidate depth, and explicitly configured the local model's context window. The technical details are in the code post-mortem in [Part 1: The Retrieval Primitives](./how-lume-works-part1-primitives.md#8-case-study-the-retrieval-bug-that-confused-the-agent).

With the fix in place, the planner can emit `"died of starvation grief"`, the death scene enters the retrieved field, and the evaluator has the evidence it needs. The answer was in the data all along; the system just had to ask from the right angle.

---

## Why This Matters

The future of search isn't a smarter text box. It is:
* **Spatial:** Meaning has structure. Show it.
* **Multi-Query:** Real questions have facets. Search all of them at once and watch where they overlap.
* **Agentic and Honest:** A system that plans, evaluates, refines, and cites — and can say when the retrieved passages do not answer the question.
* **Fast Enough to Feel Alive:** Streaming simulation coordinates so results don't just load — they warp in.

Now cue *Derezzed* and watch it jump to hyperspace. 🌌

---

> [!NOTE]
> **Go Under the Hood**
> If you want to see the code and equations behind Lume, check out our technical series:
> - [How Lume Works, Part 1: The Retrieval Primitives](./how-lume-works-part1-primitives.md) — Fusing field-aware BM25, GTR-T5 semantic vectors, and significance-weighted entity graphs.
> - [How Lume Works, Part 2: The Visualization Field](./hyperspace-search-deep-dive.md) — Projecting 768-D vectors into 3D, Weber-style layout relaxation, and Three.js rendering.

`#SearchEngine` · `#VectorSearch` · `#AgenticAI` · `#RAG` · `#DataVisualization` · `#Rust` · `#ThreeJS` · `#FutureOfSearch` · `#LLM` · `#DeepTech`
