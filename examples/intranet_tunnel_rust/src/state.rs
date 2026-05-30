use crate::error::{ProtocolError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStreamState {
    PendingAgentOpen,
    Open,
    ClientWriteClosed,
    AgentWriteClosed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStreamState {
    DialingTarget,
    Open,
    ServerWriteClosed,
    TargetWriteClosed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerTransition {
    AgentOpenSucceeded,
    AgentOpenFailed,
    ClientEof,
    AgentEof,
    FatalClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransition {
    TargetOpenSucceeded,
    TargetOpenFailed,
    ServerEof,
    TargetEof,
    FatalClose,
}

pub fn server_transition(
    state: ServerStreamState,
    transition: ServerTransition,
) -> Result<ServerStreamState> {
    use ServerStreamState::*;
    use ServerTransition::*;

    match (state, transition) {
        (PendingAgentOpen, AgentOpenSucceeded) => Ok(Open),
        (PendingAgentOpen, AgentOpenFailed | FatalClose) => Ok(Closed),
        (Open, ClientEof) => Ok(ClientWriteClosed),
        (Open, AgentEof) => Ok(AgentWriteClosed),
        (Open, FatalClose) => Ok(Closed),
        (ClientWriteClosed, AgentEof | FatalClose) => Ok(Closed),
        (ClientWriteClosed, ClientEof) => Ok(ClientWriteClosed),
        (AgentWriteClosed, ClientEof | FatalClose) => Ok(Closed),
        (AgentWriteClosed, AgentEof) => Ok(AgentWriteClosed),
        (Closed, _) => Ok(Closed),
        _ => Err(ProtocolError::InvalidTransition("server stream state").into()),
    }
}

pub fn agent_transition(
    state: AgentStreamState,
    transition: AgentTransition,
) -> Result<AgentStreamState> {
    use AgentStreamState::*;
    use AgentTransition::*;

    match (state, transition) {
        (DialingTarget, TargetOpenSucceeded) => Ok(Open),
        (DialingTarget, TargetOpenFailed | FatalClose) => Ok(Closed),
        (Open, ServerEof) => Ok(ServerWriteClosed),
        (Open, TargetEof) => Ok(TargetWriteClosed),
        (Open, FatalClose) => Ok(Closed),
        (ServerWriteClosed, TargetEof | FatalClose) => Ok(Closed),
        (ServerWriteClosed, ServerEof) => Ok(ServerWriteClosed),
        (TargetWriteClosed, ServerEof | FatalClose) => Ok(Closed),
        (TargetWriteClosed, TargetEof) => Ok(TargetWriteClosed),
        (Closed, _) => Ok(Closed),
        _ => Err(ProtocolError::InvalidTransition("agent stream state").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TunnelError;

    #[test]
    fn server_transitions_cover_close_pairing() {
        let state = server_transition(
            ServerStreamState::PendingAgentOpen,
            ServerTransition::AgentOpenSucceeded,
        )
        .unwrap();
        assert_eq!(state, ServerStreamState::Open);
        let state = server_transition(state, ServerTransition::ClientEof).unwrap();
        assert_eq!(state, ServerStreamState::ClientWriteClosed);
        let state = server_transition(state, ServerTransition::AgentEof).unwrap();
        assert_eq!(state, ServerStreamState::Closed);
        let state = server_transition(state, ServerTransition::AgentEof).unwrap();
        assert_eq!(state, ServerStreamState::Closed);
    }

    #[test]
    fn server_rejects_data_before_open_result_equivalent() {
        let err = server_transition(
            ServerStreamState::PendingAgentOpen,
            ServerTransition::ClientEof,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            TunnelError::Protocol(ProtocolError::InvalidTransition(_))
        ));
    }

    #[test]
    fn agent_transitions_cover_target_failure_and_half_close() {
        let state = agent_transition(
            AgentStreamState::DialingTarget,
            AgentTransition::TargetOpenFailed,
        )
        .unwrap();
        assert_eq!(state, AgentStreamState::Closed);

        let state = agent_transition(
            AgentStreamState::DialingTarget,
            AgentTransition::TargetOpenSucceeded,
        )
        .unwrap();
        let state = agent_transition(state, AgentTransition::ServerEof).unwrap();
        assert_eq!(state, AgentStreamState::ServerWriteClosed);
        let state = agent_transition(state, AgentTransition::TargetEof).unwrap();
        assert_eq!(state, AgentStreamState::Closed);
    }
}
