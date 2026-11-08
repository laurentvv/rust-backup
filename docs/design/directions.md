# Brand directions (J3.2) — decision gate

> Status: **decided 2026-11-08 — direction C · Arche, product name unchanged (RustBackup / rust-backup)**.
> The earlier hybrid Strata + Ferrite choice is superseded (see "Decision record"). J3.3 starts from the
> Arche definition below; J3.5 (rename) is dropped.
> Inputs: `docs/design/research/README.md` (J3.1). Visual comparison: `docs/design/directions/comparison.html`
> (also published as a private Artifact). Logo sketches: `docs/design/directions/*-mark.svg`.

All three directions share the same constraints and the same sample data (3 jobs, 411.6 MiB, 5 snapshots,
3.2× dedup) so they can be compared on equal terms.

## Shared constraints (any direction)

- **Dashboard leads with protection status** (one sentence + color + action), then time, then inventory
  (research principles 1 and 4).
- **Fluent geometry kept everywhere**: 8 px window/dialog/flyout radius, 4 px controls (Ferrite tightens to 2–3 px
  inside content, never on the window).
- **Semantic colors never reuse the accent** (success/warning/danger are their own tokens).
- **Must look complete on the Mica fallback** (inactive window, battery saver, Windows 10): every direction
  defines an opaque base color.
- **WCAG 2.2 AA**: body text ≥ 4.5:1, UI strokes ≥ 3:1 (ratios below computed from sRGB relative luminance).
- **GPUI feasibility**: `WindowBackgroundAppearance::MicaBackdrop` for B, full-color SVG marks via `img()`, embedded
  fonts via `add_fonts` (A and C), `with_animation` for state transitions. No per-element backdrop blur exists, so
  translucency is only ever the window backdrop.

---

## A · Ferrite — industrial dark-tech

**Thesis.** A backup is a magnetic write. The app reads like an instrument: monospaced numbers, condensed
all-caps headlines, one warm material color (iron oxide), and a 30-day "tape track" where each cell is a day.

- **Names**: *Ferrite* (preferred) · Magnetite · Oxyde. Ferrite is a common engineering term (trademark check
  needed; possible collisions with hardware/electronics brands and crates).
- **Mark** (`directions/ferrite-mark.svg`): a toroidal ferrite core with its air gap and three windings, on a
  graphite tile. At 16 px it reads as an open orange ring.
- **Palette** — dark first:

  | Token | Dark | Light | Use |
  |---|---|---|---|
  | base | `#0F1012` graphite | `#E9E8E4` concrete | window, nav |
  | content | `#141518` | `#F6F5F2` | content layer |
  | surface | `#17191C` forge | `#FFFFFF` | cards, rows |
  | line | `#2A2D32` steel | `#D9D7D1` | 1 px strokes |
  | text | `#E8E4DD` ash | `#17191C` ink | body |
  | muted | `#8E9196` (5.3:1) | `#5E6168` | secondary |
  | accent / primary | `#E8703A` oxide (text on it `#140C08`, 6.3:1) | `#B4441C` (white on it 5.6:1) | CTA, active nav, track |
  | accent text | `#F08A55` (7.6:1 on base) | `#B4441C` (5.0:1) | kicker, links |
  | secondary | `#7F95A8` magnetite | `#4A6175` | data series 2 |
  | ok / warn / bad | `#6CC08A` / `#E3B341` / `#F05D5D` | `#2E7D4F` / `#9A6700` / `#C42B2B` | semantics |

- **Type**: *Big Shoulders Display* 800 uppercase for page heroes and the wordmark; *JetBrains Mono* for every number
  and technical label; Segoe UI Variable for running text. Two OFL fonts to embed (~200 KB).
- **Icons**: Lucide subset via `icon_assets!`, 1.75 stroke, square caps, 18–20 px. Active nav = oxide icon + 3 px
  oxide rail on the window edge.
- **Illustration**: isometric cutaways in graphite tones with oxide edges and fine grain — vault, USB drive,
  server rack, platters. Easy to keep consistent with fixed seeds.
- **Dashboard signature**: kicker `ÉTAT · date · time` → giant "TOUT EST PROTÉGÉ" → 4-cell mono stat strip →
  30-day tape track (full = solid oxide, incremental = half cell, failure = red hatch, today outlined) → job rows.
- **Mica**: not used in content; optional Mica Alt in the title bar only. Least native of the three.
- **Risks**: the light theme is harder to make attractive; if the oxide is reduced to a lone neon accent on black it
  falls into the generic "dark tech" cliché — the oxide must stay a *material* (gradient in the mark, fills in the
  track, illustration edges).

## B · Strata — native Fluent, Mica, calm

**Thesis.** Every snapshot deposits a layer. The app melts into Windows 11: Mica in title bar and navigation,
an opaque content layer, Segoe UI Variable only, Fluent System Icons. The identity lives in the geological mark,
the sediment palette of the illustrations and the "strata" chart.

- **Names**: *Strata* (preferred) · Sédiment · Couches. "Strata" is heavily used in tech (identity, data
  companies) — trademark check needed.
- **Mark** (`directions/strata-mark.svg`): a rounded core sample of five wavy layers, newest on top
  (sky → sand → clay → slate → deep).
- **Palette**:

  | Token | Light | Dark | Use |
  |---|---|---|---|
  | base | Mica, fallback `#F3F3F3` | Mica, fallback `#202020` | window, nav, title bar |
  | content | `rgba(255,255,255,.5)` layer | `rgba(58,58,58,.3)` layer | content layer (`LayerFillColorDefault`-like) |
  | card | `#FFFFFF` | `#2B2B2B` | cards |
  | text | `#1A1A1A` | `#FFFFFF` | body |
  | muted | `#5F5F5F` | `#C5C5C5` | secondary |
  | accent | `#3554A5` lapis (7.1:1 on white) | `#9DB4F0` (6.9:1 on card) | CTA, selection |
  | sediment ramp | `#BFD3F2` `#E6C38F` `#C47E55` `#4F6B95` `#22304F` | same | mark, illustrations, charts only |
  | ok / warn / bad | `#0F7B0F` / `#9D5D00` / `#C42B1C` | `#6CCB5F` / `#FCE100` / `#FF99A4` | Fluent status colors |

- **Type**: Segoe UI Variable only, Windows 11 ramp (caption 12/16, body 14/20, subtitle 20/28, title 28/36).
  Nothing to embed.
- **Icons**: Fluent System Icons (MIT), regular at rest, filled when active, 20 px.
- **Illustration**: soft terrain cross-sections in a light 3D "clay" render, files trapped in layers like fossils.
- **Dashboard signature**: page title → hero card ("Vos fichiers sont protégés" + next run + verification, two
  actions, illustration on the right) → strata chart (one band per snapshot, thickness and length = new data,
  the full backup as the bedrock) → destinations card (disk free space, SFTP online, dedup ratio).
- **Risks**: lowest identity strength — at a glance it can look like the Windows Settings app. Needs the
  illustrations to carry personality.

## C · Arche — quiet luxury, serif, ink and brass

**Thesis.** Put what matters under shelter. A serif sentence opens the dashboard like the first line of a
ledger. Green-black ink on cool limestone, black primary buttons, brass details, a lot of air and thin rules.

- **Names**: *Arche* (preferred; French for ark and arch, close to "archive") · Abri · Havre. Rare as a software
  name, still to be checked.
- **Mark** (`directions/arche-mark.svg`): a round arch on a ground line sheltering a brass hull. Single stroke;
  stroke thickens for the 16 px icon.
- **Palette** (deliberately cool, not the cream + terracotta cliché):

  | Token | Light | Dark | Use |
  |---|---|---|---|
  | base | `#EFEEE8` limestone | `#121614` night | window, nav |
  | content | `#FAFAF7` paper | `#1A201D` | content |
  | hairline | `#DEDBD2` | `#2E3632` | rules |
  | text | `#1B2420` ink | `#ECEAE2` chalk | body, primary button fill |
  | muted | `#5F6862` (5.3:1) | `#9DA59F` | secondary |
  | accent | `#8A6A2F` brass (4.7:1 on paper) | `#D2B577` | eyebrow, emphasis, active underline |
  | ok / warn / bad | `#3E7A55` / `#946200` / `#A8322D` | `#86C29C` / `#E2B85A` / `#EE8A80` | semantics |

- **Type**: *Newsreader* (OFL, optical sizes) for display only — hero sentence, page titles, wordmark; never below
  20 px. Segoe UI Variable for all UI text. One font family to embed.
- **Icons**: Lucide subset, 1.25 stroke, round caps — the thinnest set, so 20 px minimum.
- **Illustration**: etching-style line art, one ink plus a brass wash: arch, hull, key, books.
  The hardest of the three to generate cleanly with diffusion models (thin consistent lines) — plan for
  vectorisation or hand clean-up.
- **Dashboard signature**: eyebrow date → serif sentence "Tout est à l'abri depuis 22 h 14." → summary line →
  black primary button → 30-day dot calendar (full = ink, incremental = grey, missed = red ring) → ledger list
  under a strong top rule.
- **Risks**: can read as a magazine rather than a tool; serif must never reach small UI sizes; brass at 4.7:1 is
  only just AA for body text — keep it for short emphasis.

---

## Comparison

| Criterion | A · Ferrite | B · Strata | C · Arche |
|---|---|---|---|
| Identity strength | very strong | discreet | strong |
| Windows 11 integration | opaque; Mica optional in title bar | full (Mica, Fluent ramp, radii) | good (opaque, Fluent radii) |
| Status legibility | excellent (tape track, mono numbers) | very good (hero card, strata) | very good (sentence, calendar) |
| GPUI cost | 2 fonts to embed | no font; Mica + fallback to handle | 1 serif to embed |
| Illustration production | easy (isometric) | easy (clay) | hardest (line etching) |
| Main risk | tiring in light mode; dark-tech cliché | looks like Settings | looks editorial, not a tool |
| Name | common term, check | crowded, check | rare, check |

**Recommendation (non-binding).** Ferrite for an identity recognised at first glance; Strata if feeling native
matters most. A hybrid is possible and cheap: Strata's Fluent structure and Mica navigation, with Ferrite's oxide
palette, tape track and mono numbers in the content layer. No name has been checked against trademark registries
yet — that check is part of J3.5 before any rename.

## Decision record

| Date | Decision | By |
|---|---|---|
| 2026-11-08 | Direction: hybrid Strata + Ferrite — **superseded** the same day | owner |
| 2026-11-08 | Name: **keep the original name** — RustBackup (display) / `rust-backup` (repository). No rename, no data-folder migration, no trademark check; lot J3.5 is dropped. "Ferrite", "Strata" and "Arche" remain internal names of the design directions only. | owner |
| 2026-11-08 | Direction: **100 % C · Arche** (single theme family, light + dark), replacing the hybrid. Multi-theme option (Strata/Ferrite/Arche skins) discussed and not retained. | owner |


### Chosen direction — Arche for RustBackup (working definition for J3.3)

Everything in section "C · Arche" above applies, with these adjustments for the kept name and the platform:

| Aspect | Rule |
|---|---|
| Name and wordmark | "RustBackup" set in Newsreader (display optical size), sentence case; the product is **not** called Arche. |
| Mark | Redraw the arch mark for RustBackup in J3.3: arch + ground line + hull, hull in brass. Brass/bronze also nods to "rust" (metal patina) without using the rust-orange of Ferrite. Two stroke weights: fine for >= 32 px, heavy for 16-24 px and the `.ico`. Replaces the current teal shield. |
| Window backdrop | **Opaque** limestone (light) / night (dark) base. Mica is not part of Arche; plan lot 4.1 keeps `MicaBackdrop` only if a later review shows it does not muddy the limestone. |
| Type | Newsreader for hero sentence, page titles and wordmark only (>= 20 px); Segoe UI Variable for all UI text; tabular figures for sizes and counts. Embed Newsreader (OFL) via `add_fonts`. |
| Color | Palette exactly as in the Arche table; primary button = ink fill; brass for eyebrow, emphasis, active-nav underline only (4.7:1, short text only); semantic colors separate from brass. |
| Geometry | Fluent radii kept (8 px window/dialog, 4-6 px controls); hairline rules instead of card borders where possible. |
| Icons | Lucide subset via `icon_assets!`, 1.25-1.5 stroke, round caps, 20 px minimum. |
| Dashboard | Eyebrow date -> serif status sentence -> one-line summary -> primary/secondary action -> 30-day dot calendar -> ledger list of jobs. |
| Copy tone | Calm, human: "Retrouver un fichier", "Registre", "Tout est à l'abri depuis 22 h 14." (FR) with matching EN. Final labels decided page by page in J4. |
| Illustrations (J3.4) | Etching-style line art, one ink + brass wash, light and dark variants. Highest production risk: plan vector clean-up (or SVG tracing) after generation, fixed seeds, a strict prompt kit. |

Open items for J3.3: final mark geometry and `.ico` sizes, exact dark-mode brass legibility check, whether
Mica is used at all.
