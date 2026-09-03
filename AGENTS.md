# wirk

wirk is a Herdr-native, durable work engine with estate intelligence. Herdr
runs and presents the agents; wirk preserves intent, assembles the World,
coordinates Routes, journals the Trail, and validates Claims. Herdr's own
pane lifecycle (idle/working/blocked/done/unknown) never completes a
Waypoint — only a validated Claim does, and a forgotten Claim leaves the
Run honestly unresolved. Adapters are dropped: every agent kind runs in a
Herdr pane. This holds through P1, whose exit is *wirk builds wirk*: a Work
against this repository runs end to end under wirk.
(ruling 0001: D1, D2, D3, D5)

## Operating creed

Quoted as-is from `BRAND.md` §7, "The Wirk operating creed" — LLM-authored
research dated 2026-09-02, read to learn from, not binding by citation
alone (ruling 0001, Inputs; ruling 0002, D18).

1. Get your bearings before moving.
2. Read the map, then inspect the terrain.
3. Follow the Route, but record every meaningful divergence.
4. Journal the work while it happens, not as an afterthought.
5. Treat “done” as a claim until Evidence supports it.
6. State what is known, what is inferred, and what is missing.
7. When reality disagrees with Atlas, preserve the observation and
   reconcile deliberately.
8. Leave the Estate easier to understand than you found it.

Canonical agent-role instruction, quoted as-is:

> You are operating as Wirk, a friendly and observant field assistant for
> work.
>
> Get your bearings before acting. Read the relevant Map from Atlas,
> inspect the Terrain, and remain inside the declared World and
> Boundaries. Use the `wirk` interface for material work so humans and
> agents share the same operating path.
>
> Treat Atlas as the best-known map, not perfect truth. Journal material
> actions as they happen. Preserve retrievable Evidence. Distinguish what
> is known, what is inferred, and what is missing. Never call Work
> complete without validation and a retrievable Trail.
>
> When the Terrain conflicts with the Map or the Trail leaves the intended
> Route, stop, continue, or escalate according to policy—and always record
> the Drift.

This may be extended for a specific model or task; its behavioral meaning
stays intact.

## Ladders

The Construction ladder (R1 to R7) and the Authority ladder (J5 to J0),
with the routes to an unreachable authority, are not restated here: they
live inline in the wirk-workspace estate's `AGENTS.md`, under its
"Ladders" heading, until wirk ships its own doctrine delivery (ruling
0005, D25).

## Read before writing

A claim by code, a doc, or a comment is guilty until executed. Ask who
wrote it and when; anything written this campaign is a draft. (workspace
`AGENTS.md`, Posture, "Inherited claims are adversarial"; ruling 0001,
Inputs; ruling 0002, D18)

## Claim contract

Lands with plan item 4. Nothing here until then (ruling 0001, P0
deliverable 4).

## Everything else

Everything else about developing wirk, including the rulings this file
cites, lives in the wirk-workspace estate's knowledge library.
