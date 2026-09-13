# Wirk Herdr plugin

`herdr-plugin.toml` at the repository root declares this plugin (`id =
"wirk"`). Link the checkout for local development:

    herdr plugin link /path/to/wirk

## Configure

Two one-line files in the plugin's config directory (`herdr plugin
config-dir wirk` prints it). Nothing else is configured, and nothing
outside that directory is written.

    wirk plugin init --estate /path/to/estate      # writes .../estate

`estate` is the estate root every entrypoint operates on. Without it,
`plugin/startup.sh` logs one line and spawns no `wirkd`, and the
actions say so and stop.

    echo claude > "$(herdr plugin config-dir wirk)/harness"

`harness` is which agent Herdr should start for a Wirk conversation.
Any kind Herdr can start is allowed; the plugin reads that list from
Herdr itself at the moment of use (`herdr agent start --help`, which is
where Herdr publishes the kinds — it exposes no structured listing of
them), so it neither blesses nor blocks a harness of its own accord.
With no `harness` file, `Talk to Wirk` prints the list Herdr reports
and starts nothing. Choosing is editing this file: there is no chooser
yet.

## What each entrypoint does

- `[[startup]]` (`plugin/startup.sh`): finds a live `wirkd` for the
  configured estate via `wirk wirkd ping`; spawns one detached
  (`wirk wirkd start`) only if none answers. Idempotent across a
  session restart or live handoff.
- Action `assistant`, **Talk to Wirk** (`plugin/assistant.sh`): opens a
  tab, starts the configured harness in it with `herdr agent start`,
  and sends one opening prompt naming the persona file, the configured
  estate and the `wirk` executable. That pane is a conversation with a
  person; Work is admitted from inside it, with real bindings, by the
  ordinary public `wirk` verbs. A tab, harness or prompt that did not
  come up exits non-zero, so Herdr's plugin command log records the
  action as failed rather than succeeded, and the message names the
  tab and pane left behind and how to close them. Configuration that
  has not been done yet — no estate, no harness, an unknown harness —
  exits 0 and says what to write where; nothing failed and nothing was
  created.
- Action `claim` (`pane` context): `wirk claim`, using the execution
  triple already in the pane's environment.
- Action `wirkd-status`: `wirk wirkd status --estate <root> --admin` —
  every Work's state under the configured estate. (`ping` is the
  daemon's own liveness and reports nothing about any Work.)
- `[[panes]]` `status`, split: `wirk wirkd watch --estate <root>
  --admin` — streams every current Work's journal appends, blocking,
  no loop and no sleep; the pane ends when wirkd does, or was never
  running.

`wirkd-status` and `status` name `--admin` rather than omitting a
scope. Both verbs otherwise follow the environment they run in: inside
an actor context (`WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`) they
answer as that Work, about that Work; outside one they answer
administratively. These are the operator's own view of the operator's
own configured estate, so they say which surface they read instead of
depending on whatever triple the pane inherited.

## The conversation pane

Its working directory is the plugin root, which is this repository's
root, because that is where the persona lives: `AGENTS.md`, with
`CLAUDE.md` a relative symlink to it. Harnesses that discover project
instructions discover them from their working directory, so the
persona arrives natively and is copied nowhere. It is deliberately not
the estate: an estate is a place `wirk` is pointed at, usually with
instructions of its own, and this plugin does not write into it or
into any user or repository configuration.

Not every harness reads a root instruction file, and the ones that do
disagree about which names they read, so the opening prompt names the
persona path outright rather than assuming discovery.

The pane carries no execution triple. It would otherwise: this plugin's
own process inherits the Herdr server's environment, and everything
Herdr launches inherits it in turn, so a server started inside a Run
hands its `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID` to the
conversation pane and to the harness in it — and to `wirk claim` run
from that pane. Those identify a dispatched actor operating under a
Work contract with a boundary Wirk validates, and an assistant that
borrowed them would be asserting an authority nobody granted it. So
`Talk to Wirk` passes `--env WIRK_ESTATE_ROOT= --env WIRK_WORK_ID=
--env WIRK_RUN_ID=` to `herdr tab create`. Herdr's `--env` sets; it has
no unset form. Empty is enough: the public `wirk` CLI filters a blank
triple variable out exactly as it filters an absent one, and names it
missing.

The rest of the server's environment is still inherited, including
whatever permission mode the harness takes from there — the opening
prompt is guidance, not an enforcement boundary. The enforced
boundaries are the ones `wirk` itself applies to admitted Work.

## Binary resolution

Startup hook and every action or pane command: `WIRK_BIN_PATH` if set,
else `${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/debug/wirk`.
