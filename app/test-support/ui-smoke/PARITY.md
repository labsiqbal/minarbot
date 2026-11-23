# Leptos golden screenshot gate

Build the workspace server and app, then build the Leptos bundles. App build
replaces `app/dist`, so run it before the Leptos build. Install Chromium with
`npx playwright-core install chromium` from `app/test-support/ui-smoke`.

```sh
~/.cache/minarbot-cargo-slot.sh cargo build --workspace
npm ci --prefix app
npm ci --prefix app/test-support/ui-smoke
npm run build --prefix app
~/.cache/minarbot-cargo-slot.sh cargo xtask leptos
cd app
~/.cache/minarbot-node-slot.sh npm run parity:gate -- /tmp/minarbot-parity
```

Each of 16 groups runs its Leptos smoke once against disposable data. The gate
matches 580 captures to `baselines/<group>/<pair>.png`, frozen from `820e994`.
It fails on missing or extra PNGs, failed smokes, mismatched dimensions, more
than 100 changed RGB pixels, or more than 24 pixels with a channel delta above
16. The formerly exempt ten narrow Hub baselines are Leptos captures and use
the same thresholds as every other pair.

Use `PARITY_GROUPS=calendar,automation` to limit a run. Pass an empty artifact
directory or omit it for a fresh temporary one. `MINARBOT_RUST_BINARY` can
point at another server build. `pairs/` contains baseline-left side-by-sides,
`all.tsv`, `nonidentical.tsv`, `failures.tsv`, and `summary.json`. Bounding boxes
have exclusive right and bottom edges. Functional screenshots with a
`-diagnostic.png` suffix stay with their smoke logs.

## Updating selected baselines

Review the capture and the side-by-side before copying each explicitly named
PNG from the capture run. The gate never refreshes baselines during comparison.

```sh
PARITY_GROUPS=appearance ~/.cache/minarbot-node-slot.sh npm run parity:gate -- /tmp/review-captures
npm run parity:gate -- --update /tmp/review-captures appearance/desktop-dark.png appearance/narrow-light.png
```

## Fixture reproducibility

Display-sensitive fixtures pin timestamps and UUIDs from the frozen images.
The smoke displays the original synthetic loopback URL or workspace path where
the live test uses an ephemeral port or temporary root. Assertions still use
the real disposable fixture. Capture-only text substitutions restore the
original values before the smoke continues. No baseline PNGs were replaced.

## Frozen timing budgets

Visual runs skip timing samples by default. Set the relevant benchmark
variable to run its Leptos timing assertions. Budgets use frozen React p95
times 1.2 for cold and 1.1 for warm, per [`docs/full-rust-plan.md`](../../../docs/full-rust-plan.md).
Values are in milliseconds and shown to two decimals. Code compares the
unrounded products. Sources below refer to archived NUC review artifacts.

| Timing | React cold / warm p95 | Leptos cold / warm ceiling | Source |
| --- | ---: | ---: | --- |
| Appearance | 750.5 / 21.2 | 900.60 / 23.32 | #82 pass 2 `final/review/appearance.log` |
| Automation Routines | 839 / 16.8 | 1006.80 / 18.48 | #82 pass 2 `final/exact-method/automation/receipt.json` cold, #67 `oracle-main/automation/receipt.json` warm |
| Automation Workflows | 874.2 / 16.7 | 1049.04 / 18.37 | Same automation receipts |
| Automation Calendar | 840.2 / 16.7 | 1008.24 / 18.37 | Same automation receipts |
| Conversation controls | 372.345429 / 21.9 | 446.81 / 24.09 | #82 pass 2 `final/exact-method/controls/receipt.json` |
| Composer | 508 / 7 | 609.60 / 7.70 | #82 pass 2 `final/review/conversation.log` |
| File preview | 539.9 / 36.2 | 647.88 / 39.82 | #82 pass 2 `final/review/files.log` |
| Hub | 225.7 / 129.4 | 270.84 / 142.34 | #82 pass 2 `final/review/hub/timings.json` |
| Memory | 957.2 / 61.4 | 1148.64 / 67.54 | #82 pass 3 `memory-timings/timings.json` |
| Profiles | 745.836731 / 77.8 | 895.00 / 85.58 | #82 pass 2 `final/tip-review/profiles/receipt.json` |
| Project Overview | 922.3 / 19 | 1106.76 / 20.90 | #82 pass 2 `final/review/projects/receipt.json` |
| Project Connected apps | 932.4 / 18.6 | 1118.88 / 20.46 | Same project receipt |
| Project Environment | 940.6 / 37 | 1128.72 / 40.70 | Same project receipt |
| Project MCP | 957.1 / 17.5 | 1148.52 / 19.25 | Same project receipt |
| Scoped inbox | 369.3 / 305.8 | 443.16 / 336.38 | #82 `lazy-focus/scoped/receipt.json` |
| Scoped remote | 373.5 / 307.3 | 448.20 / 338.03 | Same scoped receipt |
| Scoped upcoming | 442.4 / 314.2 | 530.88 / 345.62 | Same scoped receipt |
| Scoped workflow | 118.8 / 73.5 | 142.56 / 80.85 | #67 `oracle-main/scoped/receipt.json` at `820e994` |
| Shell prompts | 417.4 / 52.9 | 500.88 / 58.19 | #67 `oracle-main/shell/receipt.json` at `820e994` |

Use 20 samples per timing smoke, 80 for `MINARBOT_AUTOMATION_WARM_BENCH`,
and 60 for `MINARBOT_PROMPTS_BENCH`, matching the review sample sizes.
