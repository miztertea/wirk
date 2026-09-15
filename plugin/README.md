# Wirk Herdr plugin

`herdr-plugin.toml` at the repository root declares this plugin (`id =
"wirk"`).

## Install

    herdr plugin install <owner>/<repo>

Herdr fetches the repository and then runs the manifest's `[[build]]`
step, `plugin/build.sh`, inside that fresh checkout. wirk ships no
prebuilt binaries, so that step compiles one with
`cargo build --release --locked -p wirk --bin wirk` and therefore needs
a Rust toolchain (https://rustup.rs). It says so before it starts, and
if `cargo` is absent it stops with that explanation rather than
registering a plugin whose every action would fail later. The binary it
produces is what an ordinary installation runs; no `WIRK_BIN_PATH` and
no knowledge of anyone's development tree is involved.

For local development, link the checkout instead:

    herdr plugin link /path/to/wirk

Herdr does **not** run `[[build]]` for a linked plugin, so build it
yourself (`cargo build --release -p wirk`) or export `WIRK_BIN_PATH`.

## Configure

Run the plugin's **Configure Wirk** action. It opens a pane, shows what
is set, lists the harnesses Herdr reports, and writes the answers.
Everything it writes goes through `wirk plugin init`.

The same thing from a command line, inside a Herdr plugin context:

    wirk plugin init --estate /path/to/estate --harness claude
    wirk plugin init --harness-arg --model --harness-arg claude-sonnet-5
    wirk plugin show
    wirk plugin harnesses --socket "$(herdr status server | sed -n 's/^socket: //p')"

Those are three files in the plugin's config directory (`herdr plugin
config-dir wirk` prints it) — `estate` and `harness` hold one line each,
`harness-args` one argument per line. Nothing else is configured, and
nothing outside that directory is written.

`estate` is the estate root every entrypoint operates on. Without it,
`plugin/startup.sh` logs one line and spawns no `wirkd`, and the
actions say so and stop.

`harness` is which agent Herdr should start for a Wirk conversation.
Any kind Herdr can start is allowed; the plugin reads that list from
Herdr itself at the moment of use, over the session socket
(`server.agent_manifests`, Herdr's own structured listing), so it
neither blesses nor blocks a harness of its own accord. Against each
kind, `wirk plugin harnesses` also reports whether an executable of
that name is on `PATH`. Herdr decides the executable for a kind and for
a few it is not the kind's own name, so a blank there means "probably
not installed", never "Herdr cannot start it".

`harness-args` is what that harness is started with, passed through on
Herdr's own `herdr agent start ... -- <agent-args>`. Which model or
effort a conversation runs at is the operator's choice: wirk supplies
no default and names no model of its own, and an installation that set
none starts the harness with no extra arguments at all. Each
`--harness-arg` becomes one line and is passed exactly as written, so
an argument containing a space or a `*` arrives as the single argument
it was typed as — nothing re-splits or expands it. Repeating the flag
replaces the whole list; `--clear-harness-args` sets none. Which flags
are meaningful is the harness's own business, and wirk neither
validates nor interprets them.

In the **Configure Wirk** pane the arguments are typed on one line and
split on whitespace, so an argument that must itself contain a space is
the one case to set with `wirk plugin init --harness-arg` directly.
Pressing Enter there leaves the stored list as it is; entering `-` sets
none.

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
- Action `configure`, **Configure Wirk** (`plugin/configure.sh`): asks
  for the estate, the harness and the harness's own arguments in a pane
  of its own, because a plugin action takes no arguments and has no
  terminal. Every answer is written by `wirk plugin init`; the script
  itself writes nothing. With no `wirk` executable it prints the shared
  explanation of where one comes from and configures nothing.
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

One place, `plugin/wirk-bin.sh`, sourced by the startup hook, every
action, the status pane and the scripts here. In order:

1. `WIRK_BIN_PATH`, when set. Set but not executable is an error, not a
   fall-through.
2. `${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/release/wirk` — what
   the `[[build]]` step produces at install time. This is the path an
   ordinary installation uses.
3. `wirk` on `PATH`.
4. `${CARGO_TARGET_DIR:-$HERDR_PLUGIN_ROOT/target}/debug/wirk` — a
   local development build, last, so a stale debug binary never shadows
   a released one.

When none resolve, the same helper prints the four places it looked and
the three ways to get a binary, so a missing executable reads the same
whichever surface hit it. The startup hook is the one exception: it
prints a single line and exits 0, because a startup hook must not stop
the server.
