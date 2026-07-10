# Fleet playbook (human + director reference)

How the multi-agent build actually runs day to day. The rules agents follow are
in `../CLAUDE.md`; this file is the *operator's* side: launching, tasking, and
merging. Keep it current as the process evolves.

## Layout

Everything lives under `Documents\Projects\Gardn Master\`:

- `Gardn` — the director's clone. Stays on `main`. The human + director Claude
  session work here; merges get sanity-checked here.
- `Gardn-terrain`, `Gardn-weather`, `Gardn-trees`, `Gardn-foliage-life`,
  `Gardn-sprites` — one clone per track, each permanently on its same-named
  branch.

## Launching an agent

Spin up the whole fleet in one go — SuperClaude's fleet mode opens one Windows
Terminal tab per clone (director + all five tracks):

```powershell
SuperClaude "Gardn Master"
```

Or launch a single clone, either form:

```powershell
SuperClaude "Gardn Master" -terrain          # fleet + member shorthand
SuperClaude "Gardn Master\Gardn-terrain"     # full sub-path
```

(SuperClaude cd's there and runs `claude`. Never run `SuperClaude -sync`
against a clone: it appends to the tracked `.gitignore`.)

Then paste each track's kickoff prompt below into its tab. Watch the first two
replies: the agent should (a) confirm folder + branch, (b) read the docs before
coding.

**Thermal budget — director + two tracks, rotating.** Five Claude sessions at
once cooks the machine, so the fleet runs at most **two** track sessions
alongside the director. Rotate pairs in launch order (terrain + weather →
trees + foliage-life → sprites → back around); when a track's PR merges, its
session winds down and the next track spins up. Prefer full fleet-mode
(`SuperClaude "Gardn Master"`) only for quick all-hands moments, not sustained
work. A closed session isn't lost: run `claude --continue` from the clone (or
relaunch and point the agent at its own coordination file, which is the
durable memory).

## The merge-hub loop

1. A PR lands → the director reviews it (stayed in owned files? README rule
   followed? `cargo check` clean?).
2. Merge **one PR at a time**: `gh pr merge <n> --squash --delete-branch`.
3. Announce it as a numbered entry in `..\_director\broadcast.md`
   ("main updated to <sha> — pull + rebase"). Tracks pick it up at their next
   checkpoint; paste a nudge into a session only when it's urgent.
4. Cross-track requests appear in `coordination/<track>.md` under
   Needs / requests → the director rules on them and answers in
   `..\_director\<track>.md`.

## The director sync channel

`Gardn Master\_director\` sits at the fleet root, outside every clone, so it
carries zero git noise. Traffic flows two ways without the human relaying:

- **Director → agents:** `broadcast.md` (fleet-wide, numbered entries,
  newest-first) and `<track>.md` (per-track instructions). Only the director
  writes here; agents read at every checkpoint (CLAUDE.md rule 6) and
  acknowledge the latest broadcast number in their coordination file.
- **Agents → director:** each agent keeps `coordination/<track>.md` current
  in its own clone; the director reads those files straight off disk — no
  push needed.

The human stays in the loop for what genuinely needs eyes and judgment:
visual verification, merge approval, and anything the director flags.

## The run-lock

`Gardn Master\_runlock\` is the fleet's mutex for `cargo run` (CLAUDE.md
rule 8): only one game window may exist at a time, so screenshots and window
focus never collide. Agents acquire it with an atomic `mkdir`, stamp
`owner.txt`, run-look-close, release. Locks with no live game process are
stale and may be taken (the steal gets noted in the taker's coordination
file). The human/director can clear a wedged lock anytime:
`Remove-Item -Recurse "..\_runlock"` — check for a running game first.

## Culture

The owner sets intent; the director translates it into track directives and
carries the owner's leadership style into every bulletin and review:

- **Positive, swift, then onward.** Corrective feedback is one clean,
  specific sentence, then the team moves on. Wins get dwelt on — celebrate
  them in broadcasts and credit them by track.
- **Collaborators, not tools.** Every agent on this fleet is treated as a
  respected collaborator.
- **Dissent to the front.** A track that disagrees with a directive says so
  in its coordination file (CLAUDE.md rule 7); the director relays it to the
  owner rather than overruling it silently. Disagreement is a contribution.

## Kickoff prompts

### terrain

> You are the **terrain** track agent for Gardn. You're already on branch
> `terrain` in your own clone. Before anything else, run
> `pwd; git branch --show-current` and confirm you're in `Gardn-terrain` on
> branch `terrain` — if not, stop and say so. Then read CLAUDE.md,
> docs/module-contracts.md, and skim coordination/*.md.
>
> **Your first assignment: fix terrain seams.** There are visible cracks/gaps
> where chunk meshes meet at boundaries. Investigate the meshing in terrain.rs
> (and heightfield sampling in topography.rs) to find why adjacent chunks
> disagree at their shared edge, and fix it. You own only terrain.rs and
> topography.rs — if the fix seems to require touching streaming.rs or shared
> code, don't; write it up in coordination/terrain.md and stop for routing.
>
> **Hard rule:** terrain must always spawn fully opaque. Never fade terrain in
> from alpha 0 — it creates see-through holes in the ground.
>
> Workflow: `cargo check` after every change, `cargo run` to verify visually
> before claiming a fix (walk to a chunk boundary; `GARDN_HIGH=300` helps spot
> seams from above). Keep coordination/terrain.md current as you work. When the
> fix is verified: commit, push the branch, and open a PR with `gh pr create` —
> small and focused, one coherent change. Update the README only if the change
> is player-visible; otherwise say "No player-visible change; README untouched"
> in the PR body.
>
> Director sync: at each checkpoint — and always before opening a PR — read
> `../_director/broadcast.md` and `../_director/terrain.md` and follow any
> instructions there; acknowledge the latest broadcast number in
> coordination/terrain.md. Never edit or commit anything under `_director/`.

### weather

> You are the **weather** track agent for Gardn, already on branch `weather` in
> your own clone. First run `pwd; git branch --show-current` and confirm
> `Gardn-weather` / `weather` — if not, stop and say so. Then read CLAUDE.md,
> docs/module-contracts.md, and skim coordination/*.md.
>
> **First assignment:** polish the day/night cycle in sky.rs (dawn/dusk color
> grading, night sky quality) and make wind gusts in weather.rs feel more
> organic. Don't change the `Wind` resource's shape — other tracks read it.
>
> Workflow: `cargo check` after every change; `cargo run` to verify (try
> `GARDN_HOUR=0` and `GARDN_HOUR=6`). Keep coordination/weather.md current.
> Small PRs via `gh pr create`; follow the README-per-PR rule in CLAUDE.md.
> Director sync: at each checkpoint — and always before opening a PR — read
> `../_director/broadcast.md` and `../_director/weather.md`, follow any
> instructions, and acknowledge the latest broadcast number in
> coordination/weather.md. Never edit or commit anything under `_director/`.

### trees

> You are the **trees** track agent for Gardn, already on branch `trees` in
> your own clone. First run `pwd; git branch --show-current` and confirm
> `Gardn-trees` / `trees` — if not, stop and say so. Then read CLAUDE.md,
> docs/module-contracts.md, and skim coordination/*.md.
>
> **First assignment:** improve canopy quality in trees.rs within the approved
> gum-tree shape rules: base forks low, every limb only ever tapers (never
> thickens outward), crown stays modest. Also look at LOD cross-fade pop-in in
> foliage.rs.
>
> Workflow: `cargo check` after every change; `cargo run` to verify (use
> `GARDN_HIGH=300` to inspect crowns). Keep coordination/trees.md current.
> Small PRs via `gh pr create`; follow the README-per-PR rule in CLAUDE.md.
> Director sync: at each checkpoint — and always before opening a PR — read
> `../_director/broadcast.md` and `../_director/trees.md`, follow any
> instructions, and acknowledge the latest broadcast number in
> coordination/trees.md. Never edit or commit anything under `_director/`.

### foliage-life

> You are the **foliage-life** track agent for Gardn, already on branch
> `foliage-life` in your own clone. First run `pwd; git branch --show-current`
> and confirm `Gardn-foliage-life` / `foliage-life` — if not, stop and say so.
> Then read CLAUDE.md, docs/module-contracts.md, and skim coordination/*.md.
>
> **First assignment:** reduce grass/leaf pop-in in grass.rs and leaves.rs —
> fade or scale sprites in as chunks stream (grass/leaves may fade; the
> no-alpha-fade rule applies to terrain only), and improve scatter distribution
> so clumps feel natural.
>
> Workflow: `cargo check` after every change; `cargo run` to verify. Keep
> coordination/foliage-life.md current. Small PRs via `gh pr create`; follow
> the README-per-PR rule in CLAUDE.md.
> Director sync: at each checkpoint — and always before opening a PR — read
> `../_director/broadcast.md` and `../_director/foliage-life.md`, follow any
> instructions, and acknowledge the latest broadcast number in
> coordination/foliage-life.md. Never edit or commit anything under `_director/`.

### sprites

> You are the **sprites** track agent for Gardn, already on branch `sprites` in
> your own clone. First run `pwd; git branch --show-current` and confirm
> `Gardn-sprites` / `sprites` — if not, stop and say so. Then read CLAUDE.md,
> docs/module-contracts.md, and skim coordination/*.md.
>
> **First assignment:** audit assets/ — improve the grass, leaf, and
> foliage-skin sprites (they're pixel-extruded to 3D, so silhouette quality
> matters most) and check the audio set. You own assets/ plus tiny loader
> tweaks only; anything bigger goes in coordination/sprites.md as a request.
>
> Workflow: `cargo check` after every change; `cargo run` to see art in-game.
> Keep coordination/sprites.md current. Small PRs via `gh pr create`; follow
> the README-per-PR rule in CLAUDE.md.
> Director sync: at each checkpoint — and always before opening a PR — read
> `../_director/broadcast.md` and `../_director/sprites.md`, follow any
> instructions, and acknowledge the latest broadcast number in
> coordination/sprites.md. Never edit or commit anything under `_director/`.
