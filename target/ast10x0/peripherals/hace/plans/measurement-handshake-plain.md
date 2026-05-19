# Measurement handshake — plain-language explainer

*Plain companion to `measurement-orchestration-handshake.md` (the rigorous,
cited ADR). Same idea, no jargon. If anything here and the ADR disagree, the
ADR wins.*

## The setup (a kitchen analogy)

Think of the crypto/hash engine as **one shared blender** in a shared
building. Three workers are in separate locked rooms and can only pass things
through slots in the walls:

- **F** has the pantry — the flash data.
- **C** runs the blender — the hashing.
- **M** is the manager following a recipe (the PFR manifest): "blend region R
  and tell me whether it matches the expected value."

## It is a streaming job (not one press of a button)

The flash region is huge — it does not fit in the blender in one go. Hashing
it really means: scoop a bit, blend it in, scoop the next bit, blend it in…
repeat thousands of times until the whole region is done. There is no single
"press button, get answer."

So the real question is not *streaming vs one-shot* — it is streaming. The
question is **who runs that scoop-blend-repeat loop, and where.**

## The bad way (rejected)

M keeps the blender running and walks to the pantry for every single scoop,
carrying each scoop through the wall slots, **while still holding the blender
the whole time** — including during all the walking and waiting between
scoops.

Result: nobody else in the building can blend anything until M finishes the
entire vat, even though M is mostly walking around. The blender is shared, so
this starves everyone else who needs it (the other security checks). This is
the design we reject.

## The good way (the design)

M does **not** run the loop across the walls. M hands C access to the **whole
region at once** and says: "blend all of this and give me the final result."

C does the entire scoop-blend-repeat loop **inside its own room, start to
finish**, and only then passes back one answer. The blender is busy only while
C is actually blending — not during any back-and-forth.

The hashing is still streaming. The only change: the whole streaming loop
happens **in one room, in one continuous go**, instead of by passing each
scoop back and forth through a wall while holding the blender hostage.

## The catch (this is the real decision)

For C to "blend the whole vat itself," C has to be able to reach the whole
region in one shot — either M gives C a key to the whole region, or the
pantry and the blender are put in the same room.

If neither is possible, you are forced back to passing scoops through the wall
(the bad way). And you **cannot** dodge it by blending a hundred separate
little cups and stacking them: the recipe expects one blend of the whole
thing; many small blends produce a different number that will not match the
manifest.

That key-to-the-whole-region (or same-room) choice is the open decision the
ADR calls the "hinge" — see `measurement-orchestration-handshake.md`.

## One-line summary

It is a streaming hash. Because the blender is shared and must not be held
hostage across the walls, the whole streaming loop has to run on one side, as
a single job — which only works if that side can reach the whole region at
once.
