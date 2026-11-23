# Leptos golden screenshot gate

Build the React app and Leptos route bundles. Install the smoke package with
`npm ci --prefix test-support/ui-smoke` from `app/` and install its Playwright
Chromium. Run from `app/` with the workspace server built.

```sh
~/.cache/minarbot-node-slot.sh npm run parity:gate -- /tmp/minarbot-parity
```

Set `PARITY_GROUPS` to a comma-separated list of group names (for example
`PARITY_GROUPS=calendar,automation`) to run only those groups. The release
review runs all groups.

Each smoke selects its client explicitly. Groups that accept one client run
once per client. The remaining groups select React and Leptos inside the smoke.
The gate ignores inherited client overrides. The Hub group uses the Rust
hosted server unless `MINARBOT_HOSTED_HUB_BINARY` names another build.

Pass an empty output directory or omit it to use a fresh temporary directory.
`MINARBOT_RUST_BINARY` can point at a different build of the server. The gate
uses disposable servers and captures at least 580 pairs across 16 groups. Its
source smokes still run their functional assertions. Each group has a log, and
`pairs/` contains side-by-sides with React on the left, TSVs for all pairs,
nonidentical pairs, and failures, and per-group totals in `summary.json`.

Each pair must have the same dimensions. A pixel counts as changed if any RGB
channel differs. At most 100 pixels may change, and at most 24 may have a
channel delta greater than 16. In the previous review, the settled conversation
pairs peaked at 61 changed pixels and three over 16. These limits allow small
raster differences while catching visible changes. Missing captures and failed
smokes also fail the gate. The bounding box records changed pixels with an
exclusive right and bottom edge.

The Projects smoke also exercises Leptos-only project messaging and archive
restoration. Conversation controls exercise Leptos-only search across chats.
These produce diagnostic captures alongside the paired screens.

Leptos keeps top navigation on narrow Hub screens so content and composer use
the full viewport. React's narrow sidebar clips content. The gate captures and
compares the following pairs, reports them as `intentional`, and still requires
both captures with matching dimensions. [Owner decision, 2026-11-23](https://github.com/labsiqbal/minarbot/issues/82#issuecomment-7096356509)

- `hub/room-narrow-dark.png`
- `hub/room-narrow-light.png`
- `hub/members-narrow-dark.png`
- `hub/members-narrow-light.png`
- `hub/invite-narrow-dark.png`
- `hub/invite-narrow-light.png`
- `hub/invitation-narrow-dark.png`
- `hub/invitation-narrow-light.png`
- `hub/create-project-narrow-dark.png`
- `hub/create-project-narrow-light.png`
