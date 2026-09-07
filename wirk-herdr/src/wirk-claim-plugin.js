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
// runs the driver's own binary (WIRK_CLAIM_BIN below) with the
// pane's already-injected env (the execution triple and PATH --
// wirk-herdr/src/lib.rs's actor_pane) and does nothing else: no
// state beyond the child-session tracking herdr-agent-state.js
// itself keeps, no counter, no timer, no output parsing. `claim`
// with no --artifact asks wirkd for the Waypoint's declared outputs
// and claims each by name (P2.7 W1); a refused claim is state wirkd
// already journals and the run loop already acts on (0049, 0052,
// 0044) -- this plugin does not pre-check anything, it fires and
// lets wirkd judge.
//
// WIRK_CLAIM_BIN names the driver's own absolute binary path, spliced
// in by wirk-herdr/src/claim_hook.rs's wirk_claim_plugin_js at write
// time (this file, as shipped in the crate, is a template -- the
// placeholder below is never the literal value any actor pane sees).
// Invoking that exact binary, not the bare name "wirk", is what
// P2.7 Wave 2's own delivery got wrong: 0050 D151 puts the running
// binary's *directory* on PATH but never renames the file inside it
// to "wirk", so execFile("wirk", ...) is ENOENT whenever the driver
// is a preserved or renamed binary -- this estate's own review
// discipline (native-progress-contract-use/HANDOFF.md §1.4, proved
// live). execFile takes WIRK_CLAIM_BIN as a literal argv[0], with no
// shell in between, so no quoting is needed here for spaces or shell
// metacharacters in the path -- only valid-JS-string-literal escaping
// (serde_json::to_string on the Rust side; a JSON string literal is a
// valid JS one).
import { execFile } from "node:child_process";

const WIRK_CLAIM_BIN = "__WIRK_CLAIM_BIN__";

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
        execFile(WIRK_CLAIM_BIN, ["claim"], () => {});
      }
    },
  };
};
