# Looking at a Work in a browser

`wirk browser` renders a Work the way a person would ask about it:
what it is for, how far it has got, what needs attention, the World it
was given, and the evidence its Claims rest on. Everything it shows
comes from projections wirk already publishes — `wirk work status`,
`wirk world show` and `wirk artifact read` — so the page can show you
nothing those verbs would not, under the same scope.

## From a Work's own pane

With the Herdr plugin installed, the **Browser view** action on a
Work's pane opens that Work in your browser and leaves a button that
brings you back to the pane. It needs nothing else: the action works
out which Work the pane is running, starts a loopback bridge, and
points your configured browser at it.

## From a command line

    wirk browser serve --estate <root> --work <id> [--admin] [--open]

serves one Work and blocks until you stop it (Ctrl-C) or it goes
`--idle-timeout` seconds without a request (default 900). It prints the
URL it is listening on.

    wirk browser view --estate <root> [--work <id>] [--admin] --out page.html

writes one self-contained HTML file instead — a single Work with
`--work`, or the estate map without it. No server, and no return
action: the button belongs to a live bridge.

Both take the same scope flags as `wirk work status`: `--admin` for the
administrative read, `--requesting-work <id>` to read as that Work.
Inside an actor's own pane, neither is needed — the Work asks as
itself. The estate map is the administrative listing; a scoped read
answers about one named Work rather than listing the estate's Work ids.

## What the pages show

- **Work.** The stated intent as the heading, a sentence saying where
  the Work is, then anything waiting on a person: a hold, a request for
  input, a failed Run, or evidence that no longer reads at the content
  its Claim was checked against. Then progress by waypoint, the World —
  repository, branch, base commit, what the Work may write, what it owes
  — and the evidence table.
- **World.** The context this Run was actually delivered, as
  `wirk world show` prints it, with its revision chain when the actor
  expanded it. Withheld from a reader the scope does not admit, and said
  so rather than shown empty.
- **Evidence.** An artifact's own bytes, re-read and re-hashed against
  the content identity recorded when its Claim was validated. If it no
  longer matches, the page says that and shows nothing.
- **Estate.** Every Work grouped by the repository it changes, with what
  each is for and what needs attention.

Each page is a snapshot taken when you asked for it, and says so.
Nothing here polls or streams; reload to ask again. When wirk cannot be
reached, the page reports the failure and, if it had a previous answer,
shows it dated and labelled as not current rather than letting old
content pass for present state.

## Returning to Herdr

**Focus this Work's pane in Herdr** asks Herdr, at the moment you click
it, which pane is running this Work's current Run, in the Herdr session
the server was started from. If Herdr lists one — including a pane it
kept after the Run ended — that pane is focused. If it lists none,
nothing runs and the page says so. The button carries no data of its
own, so there is no target in the request for anything to change.

## What the bridge is

One process, bound to `127.0.0.1` on a port the OS picks, reachable
only with a random token in the path. It registers no URL scheme,
publishes nothing, and reads no request body, header or query: the
return action's only inputs are wirk's answer and Herdr's answer. Text
that came out of the estate is escaped wherever it is written and is
never used to build a link.
