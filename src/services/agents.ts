/**
 * Typed wrapper over the agent commands (spec sections 6, 16, 24).
 *
 * Command names and argument shapes mirror `src-tauri/src/commands/agents.rs`.
 * The backend derives the list from its agent registry, so this module never
 * names an agent itself: adding an adapter to the Rust core makes it appear here
 * with no change to this file.
 *
 * Nothing here carries a credential (spec section 17): the payload is which
 * CLIs exist on this machine and where they live.
 */

import { call } from "./backend";
import type { AgentInfo } from "../types";

export const agentsApi = {
  /**
   * Every agent this build implements, with its installation state.
   *
   * Cheap but not free (a `PATH` lookup per agent), so callers load it once
   * rather than per render.
   */
  list: (): Promise<AgentInfo[]> => call<AgentInfo[]>("list_agents"),
};
