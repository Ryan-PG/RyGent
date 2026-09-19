//! Agent commands (spec sections 6, 24).
//!
//! One command: [`list_agents`], which tells the frontend which CLI agents this
//! build can run and whether each is installed on this machine.
//!
//! The list is *derived* from the agent registry rather than declared here, so
//! the UI cannot offer an agent a session could not start - and adding an
//! adapter does not mean editing a command (spec section 6: additional CLI
//! agents must be addable without duplicating session-management logic).
//!
//! Nothing here touches a provider or a credential: this is machine
//! information (is the CLI on `PATH`, where is it), never session state (spec
//! section 17).

use crate::agents::{descriptors, AgentDescriptor};

/// Every agent this build implements, with its installation state.
///
/// Read-only and side-effect free, so the frontend can call it whenever it needs
/// to describe an agent (the workspace form, the Settings tab). Discovery is a
/// `PATH` lookup per agent, which is why the result is not cached across calls:
/// a user who installs a CLI and restarts the app gets the new answer, and the
/// call is cheap enough for a UI that refreshes it rarely.
#[tauri::command]
pub fn list_agents() -> Result<Vec<AgentDescriptor>, String> {
    Ok(descriptors())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_returns_every_supported_agent() {
        let agents = list_agents().expect("list agents");

        assert_eq!(agents.len(), crate::agents::AGENT_IDS.len());
        let ids: Vec<&str> = agents.iter().map(|agent| agent.id).collect();
        assert_eq!(ids, crate::agents::AGENT_IDS);
    }

    /// The payload the frontend receives must be describable on its own: an id
    /// to persist, a label to display, and a state to explain (spec sections 6,
    /// 16, 24).
    #[test]
    fn every_listed_agent_can_be_described_and_started() {
        for agent in list_agents().expect("list agents") {
            assert!(!agent.id.trim().is_empty());
            assert!(!agent.name.trim().is_empty());
            assert_eq!(
                agent.installed,
                agent.executable_path.is_some(),
                "{} reports an inconsistent installation state",
                agent.id
            );
            // Every listed agent is one a session can actually be started with.
            assert!(
                crate::agents::is_supported(agent.id),
                "{} is offered but not startable",
                agent.id
            );
        }
    }
}
