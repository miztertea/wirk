# Wirk Herdr plugin

`herdr-plugin.toml` at the repo root declares this plugin (`id =
"wirk"`). Link the checkout for local development:

    herdr plugin link /path/to/repos/wirk

## Configure an estate

The startup hook and the `submit`/`wirkd-status` actions no-op until an
estate root is configured:

    wirk plugin init --estate /path/to/estate

This writes the estate root as one line to
`$HERDR_PLUGIN_CONFIG_DIR/estate` (`herdr plugin config-dir wirk`
prints that directory). Without it, `plugin/startup.sh` logs one line
and exits 0 without spawning a `wirkd`.

## What each entrypoint does

- `[[startup]]` (`plugin/startup.sh`): finds a live `wirkd` for the
  configured estate via `wirk wirkd ping`; spawns one detached
  (`wirk wirkd start`) only if none answers. Idempotent across a
  session restart or live handoff (ruling 0032 D99).
- Actions: `submit` (`wirk work submit`), `claim` (`wirk claim`,
  using the execution triple already injected into the pane's
  environment), `wirkd-status` (`wirk wirkd ping` — wirkd has no
  separate `status` verb, only `start`/`stop`/`ping`).
- `[[panes]]` `status`, `placement = "split"`: `wirk wirkd watch
  --estate <root>` (item B, ruling 0044) — streams every current
  Work's journal appends, blocking, no loop and no sleep; the pane
  ends when wirkd does (or was never running).

Binary resolution (startup hook and every action/pane command):
`WIRK_BIN_PATH` if set, else
`${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/debug/wirk`.
