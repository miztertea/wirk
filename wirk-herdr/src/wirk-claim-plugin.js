// wirk's own opencode plugin (P2.7 Wave 2). Delivered via
// OPENCODE_CONFIG naming this file in a "plugin" array -- never
// written into the actor's worktree, never under ~/
// (knowledge/work/p2-plugin-surface/orient/reorient.md §6 item 1).
// Coexists with ~/.config/opencode/plugins/herdr-agent-state.js and
// the owner's own opencode.json (opencode merges config and runs
// "all hooks run in sequence" -- measured live,
// knowledge/evidence/p2-plugin-surface-2026-09-05/w2-probe.md).
//
// Modelled on herdr-agent-state.js's own "event" dispatch: on the
// root session's session.idle (opencode's turn-end signal, the same
// case herdr-agent-state.js reads at its own session.idle case),
// runs `wirk claim` with the pane's already-injected env (the
// execution triple and PATH -- wirk-herdr/src/lib.rs's actor_pane)
// and does nothing else: no state beyond the child-session tracking
// herdr-agent-state.js itself keeps, no counter, no timer, no output
// parsing. `wirk claim` with no --artifact asks wirkd for the
// Waypoint's declared outputs and claims each by name (P2.7 W1); a
// refused claim is state wirkd already journals and the run loop
// already acts on (0049, 0052, 0044) -- this plugin does not
// pre-check anything, it fires and lets wirkd judge.

import { execFile } from "node:child_process";

export const WirkClaimPlugin = async () => {
  const childSessions = new Set();

  return {
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const sessionID =
        typeof properties?.sessionID === "string" ? properties.sessionID : undefined;

      const info = properties.info;
      if (info?.id && info.parentID) {
        childSessions.add(info.id);
      }
      if (sessionID && childSessions.has(sessionID)) {
        return;
      }

      if (type === "session.idle") {
        execFile("wirk", ["claim"], () => {});
      }
    },
  };
};
