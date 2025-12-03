# rptp Design Philosophy

This document is an *optional* deeper framing for collaborators who want more context than `CONTRIBUTING.md`.
This is not an authoritative document or strict collaboration guideline.
It exists to capture early motivations and abstract ideas behind the project’s direction, especially from its early experimental phase.

If you want a technical walkthrough of how the code is structured, start with `docs/architecture-overview.md`.

---

## Why This Document Exists

Our mental models shape the systems we build—whether we reflect on them consciously or not.
This project tries to surface those assumptions early, because making them discussable tends to improve both the code and the collaboration around it.

This is not meant as a “correct worldview”, but as an invitation: if you’re curious, we can treat design as something we observe, question, and refine together—especially in a complex domain like time synchronization.

David West’s critique of “computer thinking”, Eric Evans’ discipline of a ubiquitous language, and Freeman & Pryce’s test-guided feedback loops all point in a similar direction: building good software is an ongoing discovery.

---

## The Modeling Stance

### Meaning lives in behaviour

In `rptp`, time synchronization is treated less as a sequence of procedures acting on passive data, and more as an interaction between collaborating domain objects over time.
Messages are not just DTOs: they carry meaning in how they collaborate, qualifiy, route, and talk to by ports and clocks.

This is closer to the early “objects communicate by sending messages” view of object orientation (in the Alan Kay sense) than to “data plus attached procedures”.

We try to model PTP in terms of what things *do* in context (send, listen, qualify, track, discipline), not in terms of data bags that are pushed through procedures.
Structure is important, but we prefer structure that *falls out of behaviour* rather than structure-first design that later needs constant defensive glue.

This project is still early, and the claims in this document are hypotheses, not doctrine.
The code and tests should be the loudest expression of the stance, and this document should evolve when they disagree.

### Essence vs periphery

The core domain is where PTP meaning should remain legible: time representation, message behaviour, BMCA decisions, port roles, and clock discipline.
Infrastructure concerns—sockets, async runtimes, OS clocks, hardware timestamping—belong at the edges and should be replaceable.

This is why `crates/rptp` is a portable core, and the Tokio/embedded layers adapt it rather than “being” the implementation.

---

## What This Tends To Produce (In This Repo)

- Explicit boundaries: a small set of domain‑named traits and ingress surfaces, rather than domain code depending on a specific runtime.
- Behaviour-rich state types: port behaviour lives in dedicated role types and transitions are expressed explicitly, rather than as scattered flag checks.
- Test-guided discovery: tests are not just guard rails; they are a way to sharpen meaning, spot contradictions, and keep refactors safe.
- Smaller types and shorter methods: behaviour is pushed into focused objects instead of accumulating in large grab-bag structures.
- Flatter control flow: fewer nested `if`/`match` ladders and fewer “what state am I in?” checks scattered across the codebase.
- Less temporal coupling: fewer multi-step init/startup sequences and fewer half-initialized objects.

`docs/architecture-overview.md` shows the concrete shape of these ideas in the code.

---

## A Note On Tone

This project started partly as an experiment: how much of David West’s “Object Thinking” can be brought into systems programming without losing rigor.
If that framing resonates, great. If it doesn’t, it’s still possible to contribute effectively: the practical contribution expectations live in `CONTRIBUTING.md`.

---

## References & Influences

These are some classics that shaped the project and its design philosophy.

- **Object Thinking** – David West  
  Still one of the most important books on Object-Orientation. Not about coding. About thinking.
  Metaphors as the bridge to the unfamiliar; software as a play of collaborating actors on stage.

- **Alan Kay on “objects and messages”**  
  Especially his “objects communicate by sending messages” framing (e.g. *The Early History of Smalltalk*), which is closer to what we mean here than “classes with data and methods”.

- **Domain-Driven Design** – Eric Evans  
  The classic blue book. Somewhat heavy, but invaluable for learning to listen to the domain.
  Ubiquitous language isn’t a feature — it’s a discipline.
  Making implicit concepts explicit; building shared understanding of a domain.

- **Growing Object-Oriented Software, Guided by Tests** – Steve Freeman & Nat Pryce  
  A deeply practical book that shows how to grow well-structured, object-rich systems from the outside in — one test at a time.
  It's what David West described as “Simulation drives object discovery” in practice.
  Other ideas borrowed from this book: the value of constructor injection and encapsulating collections behind a surface of domain-meaning.

- **Elegant Objects (Vol. 1 & 2)** – Yegor Bugayenko  
  Opinionated — but all the better for it.
  A solid provocation against procedural, data-centric “OO”, with pressure toward small, focused, meaningful objects.
  A core idea borrowed from this book: vertically stacking and composing behaviour with composable decorators.

---

## Appendix: Metaphors as a Bridge to the Unfamiliar

David West wrote in *Object Thinking* about "metaphors as the bridge to the unfamiliar".
They can guide design and thinking about both the domain and code to build a shared understanding.
This appendix is intentionally optional; skip it if you prefer a purely technical read.

> 👽 **A movie metaphor for port states**
>
> Think of the Alien movies.
> Ellen Ripley is the prime actor, she has identity and agency, which is, well, to survive.
> Depending on the scene (the context), she steps into different roles, or capabilities.
> In the hangar bay, she steps into the power loader to fight the alien queen.
> In another context, she steps into a space suit to survive the cold vacuum of space.
> But whatever the context, it's always Ellen Ripley in there, same identity, same agency, different capabilities.
>
> Ports in `rptp` have identity and agency as well.
> They provide a grandmaster representation to the outside world, and present a view of the outside world to the local clock.
> In order to fulfil that responsibility, a port decides to step into `ListeningPort`, `MasterPort`, or other roles, depending on the context.
> The context?
> The scene?
> Provided by BMCA.
>
> Once in a specific role, a port never asks "In which state am I, and what shall I do then"?
> The same way Ripley never asks "Am I currently wearing the power loader or the space suit? Am I fighting the alien queen or am I doing a space walk?".
> Neither does the alien ask Ripley about her current outfit mid-scene.

> 🐣 **A zoology metaphor for constructor injection**
>
> Some animals are *precocial*: once hatched, they can walk, swim, and feed themselves almost immediately.
> Others are *altricial*: they hatch helpless and need care before they can function on their own.
>
> We prefer **"precocial objects"**: once constructed, they’re ready to do their job.
> We don’t want **“altricial objects”** that need post‑construction nursing (e.g. a sequence of startup, init, or setter calls and state checks) before they become valid collaborators.
