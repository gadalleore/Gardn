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

## The merge-hub loop

1. A PR lands → the director reviews it (stayed in owned files? README rule
   followed? `cargo check` clean?).
2. Merge **one PR at a time**: `gh pr merge <n> --squash --delete-branch`.
3. Broadcast to every *other* live session: "main updated —
   `git pull origin main`, then `git rebase main`."
4. Cross-track requests appear in `coordination/<track>.md` under
   Needs / requests → the director rules on them and writes instructions for
   the owning track.

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
