use crate::error::OaatError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Discovery,
    Handshake,
    Idle,
    Negotiation,
    Streaming,
    Paused,
    Stopped,
    Disconnected,
}

impl SessionState {
    pub fn can_transition_to(self, next: Self) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Discovery, Handshake)
                | (Handshake, Idle)
                | (Handshake, Disconnected)
                | (Idle, Negotiation)
                | (Idle, Disconnected)
                | (Negotiation, Streaming)
                | (Negotiation, Idle)
                | (Negotiation, Disconnected)
                | (Streaming, Paused)
                | (Streaming, Stopped)
                | (Streaming, Disconnected)
                | (Paused, Streaming)
                | (Paused, Stopped)
                | (Paused, Disconnected)
                | (Stopped, Negotiation)
                | (Stopped, Idle)
                | (Stopped, Disconnected)
                | (Disconnected, Discovery)
        )
    }

    pub fn transition(self, next: Self) -> Result<Self, OaatError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(OaatError::InvalidStateTransition { from: self, to: next })
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery => write!(f, "Discovery"),
            Self::Handshake => write!(f, "Handshake"),
            Self::Idle => write!(f, "Idle"),
            Self::Negotiation => write!(f, "Negotiation"),
            Self::Streaming => write!(f, "Streaming"),
            Self::Paused => write!(f, "Paused"),
            Self::Stopped => write!(f, "Stopped"),
            Self::Disconnected => write!(f, "Disconnected"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions() {
        let s = SessionState::Discovery;
        let s = s.transition(SessionState::Handshake).unwrap();
        let s = s.transition(SessionState::Idle).unwrap();
        let s = s.transition(SessionState::Negotiation).unwrap();
        let s = s.transition(SessionState::Streaming).unwrap();
        let s = s.transition(SessionState::Paused).unwrap();
        let s = s.transition(SessionState::Streaming).unwrap();
        let s = s.transition(SessionState::Stopped).unwrap();
        let s = s.transition(SessionState::Disconnected).unwrap();
        assert_eq!(s, SessionState::Disconnected);
    }

    #[test]
    fn invalid_transition() {
        let s = SessionState::Idle;
        assert!(s.transition(SessionState::Streaming).is_err());
    }
}
