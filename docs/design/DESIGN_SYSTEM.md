# RustBackup design system — Arche

> Source of truth for the visual identity chosen in J3.2 (`docs/design/directions.md`, direction C · Arche,
> product name unchanged). Code: `src/theme.rs` (tokens + gpui-kit mapping), `src/main.rs` (assets, fonts).
> Every page redesigned in J4 uses only what is defined here.

## Principles

1. **Status first.** Each page opens with a sentence that says what is true ("Tout est à l'abri depuis 22 h 14."),
   then time, then inventory.
2. **Quiet surfaces, strong rules.** Opaque limestone/night layers, hairline rules instead of card boxes, a lot of air.
3. **One accent, used sparingly.** Brass marks emphasis (eyebrow, active underline, focus ring, links). It never fills
   a button and never carries status.
4. **Serif for voice, sans for work.** The display serif speaks (titles, hero sentence); every control, label and
   number uses the system UI font.

## Color tokens

Defined once in `src/theme.rs` (`LIGHT`, `DARK`, `palette(dark)`); contrast is enforced by unit tests
(`theme::tests`, WCAG 2.2 relative luminance).

| Token | Light | Dark | Use |
|---|---|---|---|
| `base` | `#EFEEE8` limestone | `#121614` night | window, navigation, title bar |
| `content` | `#FAFAF7` paper | `#1A201D` | content layer, popovers, dialogs |
| `hairline` | `#DEDBD2` | `#2E3632` | rules, quiet borders |
| `control_border` | `#84877F` | `#66706A` | inputs, checkboxes (≥ 3:1 on base and content) |
| `surface` | `#E6E4DC` | `#252C28` | secondary buttons, hover rows, skeletons |
| `text` | `#1B2420` ink | `#ECEAE2` chalk | body text, primary button fill |
| `muted` | `#5F6862` | `#9DA59F` | secondary text (≥ 4.5:1 on every layer) |
| `brass` | `#82642B` | `#D2B577` | eyebrow, emphasis, active underline, focus ring, links — short text only |
| `brass_wash` | `#EFE7D3` | `#3A3322` | selected row / text selection |
| `ok` / `warn` / `bad` | `#37704C` / `#8C5D00` / `#A8322D` | `#86C29C` / `#E2B85A` / `#EE8A80` | semantics only |

Deviation from the J3.2 table: light `brass`, `ok`, `warn` and `control_border` were darkened slightly
(`#8A6A2F`→`#82642B`, `#3E7A55`→`#37704C`, `#946200`→`#8C5D00`) so they also pass AA on the limestone base,
not only on paper.

### gpui-kit mapping (summary)

- `background`, `sidebar`, `title_bar`, `status_bar`, `tab_bar` = `base`; `popover`, `tab_active` = `content`.
- `border`, `*_border`, `chart_grid` = `hairline`; `input` = `control_border`.
- `primary`, `button_primary`, `progress_bar`, `switch`, `caret` = `text` (ink fill), foreground = paper (light) /
  night (dark).
- `secondary`, `muted`, `accent`, `sidebar_accent` = `surface`.
- `ring`, `link`, `list_active_border`, `table_active_border` = `brass`; `selection`, `list_active`,
  `table_active` = `brass_wash`.
- `success` / `warning` / `danger` = `ok` / `warn` / `bad`.
- Charts: `chart_1` ink, `chart_2` brass, `chart_3` muted, `chart_4` ok, `chart_5` warn.

## Typography

| Role | Font | Size / weight | Notes |
|---|---|---|---|
| Hero sentence | Newsreader 16pt | 34–40 px / 400 | dashboard and empty states |
| Page title | Newsreader 16pt | 26–28 px / 400 | one per page |
| Wordmark | Newsreader 16pt | 20 px / 500 | "RustBackup", sentence case |
| Eyebrow | system UI | 12 px / 600, letter-spacing +0.06 em, uppercase | brass |
| Body / controls | system UI | 14 px / 400 | gpui-kit default |
| Secondary | system UI | 13 px / 400 | `muted` |
| Numbers | system UI | tabular figures | sizes, counts, durations |

- `theme::DISPLAY_FONT` = `"Newsreader 16pt"`; files `assets/fonts/Newsreader16pt-{Regular,Medium}.ttf`
  (static instances, SIL OFL 1.1 — `assets/fonts/OFL.txt`; source: github.com/productiontype/Newsreader at
  commit `cfcb4f7`), registered with `cx.text_system().add_fonts(..)` before the window opens.
- The serif **never** goes below 20 px and never appears in controls, tables or inputs.

## Geometry and elevation

- Radii: controls 6 px (`theme.radius`), dialogs/popovers/window 8 px (`theme.radius_lg`).
- Spacing scale (px): 4, 8, 12, 16, 24, 32, 48, 64. Page gutter 32 px; section gap 32 px; row height 44 px.
- Elevation: none in content (hairlines instead). Shadows only on popovers, menus and dialogs (gpui-kit default).
- Window backdrop: opaque `base`. Mica is not used (decision J3.2; revisit only after a visual review in 4.1).

## Mark and icon

| File | Use |
|---|---|
| `assets/brand/mark-light.svg`, `mark-dark.svg` | heavy stroke (5.5/64) — 16–24 px: sidebar, title bar, tray |
| `assets/brand/mark-fine-light.svg`, `mark-fine-dark.svg` | fine stroke (3/64) — 32 px and above: About, onboarding, empty states |
| `assets/brand/app-tile.svg` | application tile: ink square, chalk arch, brass hull (readable on light and dark taskbars) |
| `assets/app.ico` | built from the tile by `scripts/make_brand_icons.py` (16–256 px; ≤ 24 px use a heavier stroke) |

Embedded asset paths: `brand/mark-light.svg`, `brand/mark-dark.svg`, `brand/mark-fine-light.svg`,
`brand/mark-fine-dark.svg`, `brand/app-tile.svg`. Pick the light/dark file from the current theme mode.
Clear space around the mark: one arch-post width. Never recolor the hull with anything but brass.

Regenerate the icon after editing the tile:

```bash
uv run --no-project --with pillow python scripts/make_brand_icons.py
```

## Iconography

- Lucide subset embedded through `icon_assets!` (`src/main.rs`); add only icons that are used.
- Rendered at 20 px minimum (16 px only inside dense tables); color = `text` or `muted`, never brass except the
  active navigation item.

## Motion

- Durations: 120 ms (hover/press), 200 ms (page and panel transitions), 300 ms (status changes).
- Easing: ease-out for entrances, ease-in-out for state changes; no bounce.
- Respect "reduce animations" (setting added in 4.9): transitions become instant.

## Illustration (J3.4)

- Style: etching / engraving line art, one ink color plus a single brass wash, paper-white or night background,
  generous negative space, no text inside images.
- Subjects: arch, hull (ark), key, bound ledgers, crates, a doorway, a lantern — calm, archival, never "cyber".
- Production: `C:\GIT\generator-assets` (see AGENTS §7 illustration pipeline): fixed seeds, English prompts,
  light and dark variants, background removal, 2× upscale, then manual clean-up or SVG tracing for line weight
  consistency. Export PNG @1x/@2x to `assets/illustrations/`.
- Prompt kit (base):
  `etching illustration, fine engraved crosshatch lines, single dark green-black ink, subtle brass gold wash
  accent, {subject}, centered, wide margins, off-white paper background, no text, no frame, minimal, archival
  book plate style`
  Dark variant: replace the background with `deep green-black night background, chalk white lines`.

## Copy tone

Calm, human, precise. Prefer verbs a person would say: "Retrouver un fichier" / "Find a file", "Registre" /
"Ledger", "Tout est à l'abri" / "Everything is sheltered". Final labels are set page by page in J4 and live in
`src/i18n.rs`.
