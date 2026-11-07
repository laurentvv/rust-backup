# Design research (J3.1) — moodboard and principles

> Status: research input for the v2 visual identity (plan lot J3.1, 2026-11-07).
> It feeds `docs/design/directions.md` (J3.2). Nothing here is a decision yet.
> Images are linked, not copied: every reference below points at its source so the
> moodboard stays reproducible and free of third-party binaries in the repository.

## 0. Starting point — where the current UI stands

Screenshots of the v1 GUI live in the main checkout under `assets/screenshots/01..08` (dashboard, jobs, run,
restore, history, settings light/dark, dashboard dark).

What they show (observed on `01-dashboard.png` and `08-dashboard-dark.png`):

| Observation | Consequence |
|---|---|
| Generic "admin template" layout: sidebar + 4 equal stat cards + one list card | No identity; could be any CRUD app |
| Stat cards show *counts* (jobs, snapshots) instead of the one answer users want: *am I protected?* | Dashboard must lead with a protection status, not inventory |
| History is 6 identical full-width green bars at ~100 % | Bar length encodes nothing; wasted chart |
| 70 % of the window is empty below the fold | Layout does not scale to a 1100×780 window |
| Teal shield + database + orange arrow brand mark | Busy at 16 px, "generic security clip-art" |
| Flat opaque navy (dark) / grey-blue (light) surfaces | No Mica, no layering — does not feel native to Windows 11 |
| Passphrase warning hidden in sidebar footer, 11 px amber | Most important risk is the least visible element |

## 1. Platform: Windows 11 / Fluent 2

Sources (fetched 2026-11-07):

- Mica material — <https://learn.microsoft.com/en-us/windows/apps/design/style/mica> (doc commit `1b116d5`)
- Geometry — <https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/geometry> (`dea2310`)
- Typography — <https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/typography> (`7814a98`)
- Layering and elevation — <https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/layering> (`ff6bf10`)
- Fluent 2 color tokens — <https://fluent2.microsoft.design/color-tokens>
- Fluent UI System Icons — <https://github.com/microsoft/fluentui-system-icons> (MIT; npm `@fluentui/svg-icons`)

Facts extracted (the numbers we will reuse):

- **Mica** is an *opaque* backdrop sampled once from the wallpaper; use it as the **base layer only, once per window**,
  visible in the title bar (extend content into the non-client area). It falls back to a solid color
  (`SolidBackgroundFillColorBase`) when transparency is off, on battery saver, on low-end hardware, **when the window
  is inactive**, and below build 22000. → Our design must look finished on the solid fallback too.
- **Mica Alt** tints more strongly; meant for tabbed title bars with a *commanding layer* in between.
- **Two-layer model**: base layer (navigation, commands) on Mica; content layer on top with a low-opacity fill
  (`LayerFillColorDefaultBrush`), either contiguous ("standard") or segmented ("card pattern").
- **Radii**: 8 px for top-level/transient surfaces (window, dialog, flyout), 4 px for in-page controls and bars
  (button, list backplate, progress bar), 0 px where straight edges meet or when maximized.
- **Elevation** (shadow value / 1 px stroke): window & dialog 128, flyout 32, tooltip 16, card 8, control 2, layer 1;
  pressed controls drop to 1. Shadows signal hierarchy, never decoration.
- **Type**: Segoe UI Variable (weight 300–700 + automatic optical size). Ramp: caption 12/16, body 14/20,
  body strong 14/20 semibold, body large 18/24, subtitle 20/28, title 28/36, title large 40/52, display 68/92.
  Minimums 12 px regular / 14 px semibold. Sentence case everywhere; semibold instead of bold; no italic in UI.
- **Fluent 2 color**: three tiers (global ramp → alias tokens such as `colorNeutralBackground1`,
  `colorBrandForeground1` → components). Brand = a 16-step ramp (10–160) generated from one key color.
- **Fluent System Icons**: regular + filled, sizes 16/20/24/28/48, MIT. Filled = selected/active state.

GPUI constraints that frame all of this (from the repo's pinned gpui-kit 0.9.0 docs and plan §J3/J4):
`WindowBackgroundAppearance::MicaBackdrop` exists (opaque fallback needed on Windows 10 / build < 22621);
full-color SVG through `img()`; fonts can be embedded (`add_fonts`); animations via `with_animation`;
no CSS-style backdrop blur on arbitrary elements → translucency only comes from the window backdrop.

## 2. Best-looking Windows apps

| App | Link | What to take | What to avoid |
|---|---|---|---|
| Windows Settings | built-in | Left nav on Mica, content as *cards of rows* (title + description + control on the right); breadcrumb page titles | Its density: too many equally weighted rows |
| Windows Terminal | <https://github.com/microsoft/terminal> | Mica/acrylic in the tab strip only, content stays opaque & legible | — |
| Dev Home | <https://github.com/microsoft/devhome> | Dashboard of *widgets* with live state; hero banner with illustration on first run | Widget sprawl |
| PowerToys | <https://github.com/microsoft/PowerToys> | Per-utility hero image + one-sentence explanation at the top of each settings page; colorful 3D Fluent icons | Settings-only, no status overview |
| Files | <https://files.community> | Mica / Mica Alt / acrylic user choice; tabs in title bar; v4 refreshed, richer logo | Over-customization |
| Microsoft Store | built-in | Large hero art, generous 8 px cards, strong typographic hierarchy | Commercial clutter |
| Notepads | <https://github.com/0x7c13/Notepads> | Proves a tiny app can feel premium through restraint + acrylic + one accent | — |
| Ambie | <https://github.com/jenius-apps/ambie> | Illustration-led, calm, rounded; emotional tone in a utility | — |
| Screenbox | <https://github.com/huynhsontung/Screenbox> | Clean Fluent media UI, content-first | — |
| Zed (GPUI) | <https://zed.dev> | **Same rendering engine as us**: crisp 1 px borders, dense but calm, theme-able, fast transitions | IDE density is wrong for a backup app |

Observed pattern: the apps that feel *native and premium* all (1) put Mica in the title bar and nav only,
(2) keep content opaque and high-contrast, (3) use **one** accent color, (4) give each page a hero sentence.

## 3. Backup-app UX benchmark

| Product | Link | Pattern worth stealing | Weakness to beat |
|---|---|---|---|
| Arq 7 | <https://www.arqbackup.com> · review <https://www.pcworld.com/article/1953731/arq-7-backup-review.html> | Native (not Electron) UI praised as "super clean"; destinations configured in the main window | Restore buried: drill into backup → date before restore appears |
| Backblaze Personal | <https://www.backblaze.com/blog/announcing-backblaze-computer-backup-v8-5/> | One-screen status: "You are backed up as of …", one big button; v8.5 decluttered both clients | Progress detail too thin (user requests on r/backblaze) |
| Kopia UI | <https://kopia.io> · <https://kopia.discourse.group/t/upcoming-changes-to-snapshot-creation-flow-in-kopia-ui/307> | Snapshot-size **estimate before** creating; policy editing inline in the creation flow | Electron shell, tabular and technical; policies separate from snapshots |
| Duplicati 2.2 (ngclient) | <https://duplicati.com/articles/duplicati-2-2-new-stable-release> | Full rewrite to a cleaner wizard (stable Oct 2025) | Web UI in a browser tab; DST/next-run bugs reported |
| Vorta | <https://vorta.borgbase.com/usage/> | Archives tab: extract, mount, check, **diff two archives**, prune; "Next backup" shown right in the schedule tab | Qt utilitarian look |
| Time Machine | built-in macOS | The **timeline metaphor**: browse the past as layers / a time axis; restore in place | Opaque about what is in each snapshot |
| Macrium Reflect X | <https://askleo.com/lets-talk-about-macrium-reflect-x/> | Moved to sidebar navigation (8.1.8110 → X); resumable backups | Interface "changed very little" — dense, pro-tool look |
| Veeam Agent | <https://www.veeam.com/windows-endpoint-server-backup-free.html> | Tray-first: status lives in the notification area; Control Panel shows a per-day bar chart green/amber/red | Old WinForms visuals |

Principles that fall out of the benchmark:

1. **Lead with one sentence of truth**: "Protected · last backup 2 h ago · next tonight 22:00" (Backblaze) — or
   "Action needed" with the single fix.
2. **Time is the primary axis** of a backup product (Time Machine, Veeam per-day bars). The history chart should be a
   calendar/timeline of runs colored by outcome, not identical bars.
3. **Restore must be one click from the dashboard** (beats Arq's drill-down).
4. **Estimate before acting** (Kopia): show size/file count before the first full run.
5. **Show the destination physically**: disk connected? free space? server reachable? (USB/SFTP is our core.)
6. **Verification is a trust signal** — surface "last verified" as prominently as "last backed up".

## 4. Premium references (outside the category)

| Product | Link | Lesson |
|---|---|---|
| Linear | <https://linear.app/now/how-we-redesigned-the-linear-ui> · <https://linear.app/blog/behind-the-latest-design-refresh> | Theme generated in **LCH from 3 inputs** (base, accent, contrast) instead of 98 variables; elevation derived from lightness; Inter Display for headings. "Don't compete for attention you haven't earned." |
| Raycast | <https://raycast.com/blog/a-fresh-look-and-feel> | An icon built on **one strong physical metaphor** (keycap); explored glass cubes etc. before choosing; separate icons for debug builds |
| 1Password 8 ("Knox") | <https://1password.com/blog/1password-8-for-mac> | A named design language unifying platforms; criticised for wasting desktop space → keep desktop density |
| Proton Pass | <https://proton.me/pass> | Dark violet brand in a security product: color can carry trust without being blue |
| Tailscale | <https://tailscale.com> | Technical product, warm off-white + black, almost no color — trust through calm and precision |
| Things 3 | <https://culturedcode.com/things/> | Whitespace, one blue accent, delightful micro-animations on completion |

## 5. Extracted principles for our identity

1. **Status first, inventory second.** The dashboard answers "am I safe?" in < 1 s, color + sentence + illustration.
2. **Native on Windows 11**: Mica in title bar + nav, content layer opaque; 4/8 px radii; Segoe UI Variable as
   the workhorse even if a display face carries the brand.
3. **One accent, semantic colors reserved** for success/warning/danger. Never reuse the accent as "success".
4. **Time as the visual spine** (timeline / strata / calendar of runs) — it is the core metaphor of backups.
5. **Physical metaphor for the mark** (Raycast lesson): a single object that reads at 16 px.
6. **Theme from few inputs** (Linear): define base, accent, contrast and derive the ramps — keeps light/dark parity.
7. **Trust signals visible**: encryption, verification, lock state shown as calm chips, not hidden footers.
8. **Must survive Mica fallback** (inactive window, battery saver): no design that depends on translucency.
9. **Illustration with a job**: empty states, onboarding, outcomes — never decorative filler (generated locally
   with `generator-assets`, consistent prompt kit).
10. **Accessibility AA**: 4.5:1 body text, 3:1 UI strokes, focus rings visible on Mica, reduced-motion setting.

## 6. Skills synced for the next lots

Copied (git-ignored) into `.agents/skills/` of the main checkout from the asset kit:
`design-taste`, `oil-ui`, `prototype`, `emil-design-eng`, `taste-accessibility`, and genre packs
`dark-tech`, `quiet-luxury` (matching directions A and C).

## 7. Limits of this research

- Screenshots of third-party apps were not copied into the repo (licensing and size); links only.
- Backblaze/Arq/Macrium details come from vendor blog posts and press reviews dated 2022–2026, not hands-on use.
- Raycast palette values (e.g. `#ff6363`) come from third-party summaries, not official docs.
