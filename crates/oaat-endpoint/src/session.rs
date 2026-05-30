use oaat_core::error::OaatError;
use oaat_core::message::{EndpointCapabilities, HelloAck, Message};
use oaat_core::session::SessionState;

pub struct EndpointSession {
    pub state: SessionState,
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub controller_id: Option<String>,
    pub stream_id: Option<String>,
}

impl EndpointSession {
    pub fn new(endpoint_id: String, endpoint_name: String) -> Self {
        Self {
            state: SessionState::Discovery,
            endpoint_id,
            endpoint_name,
            controller_id: None,
            stream_id: None,
        }
    }

    pub fn transition(&mut self, next: SessionState) -> Result<(), OaatError> {
        self.state = self.state.transition(next)?;
        Ok(())
    }

    pub fn handle_hello(
        &mut self,
        msg: &Message,
        capabilities: EndpointCapabilities,
        audio_port: u16,
        clock_port: u16,
        buffer_ms: u32,
    ) -> Result<Message, OaatError> {
        match msg {
            Message::Hello(hello) => {
                if hello.protocol_version != oaat_core::PROTOCOL_VERSION {
                    return Err(OaatError::VersionMismatch {
                        expected: oaat_core::PROTOCOL_VERSION,
                        got: hello.protocol_version,
                    });
                }
                self.controller_id = Some(hello.controller_id.clone());
                self.transition(SessionState::Handshake)?;
                let ack = Message::HelloAck(HelloAck {
                    protocol_version: oaat_core::PROTOCOL_VERSION,
                    endpoint_id: self.endpoint_id.clone(),
                    endpoint_name: self.endpoint_name.clone(),
                    capabilities,
                    audio_port,
                    clock_port,
                    buffer_size_ms: buffer_ms,
                });
                self.transition(SessionState::Idle)?;
                Ok(ack)
            }
            _ => Err(OaatError::InvalidStateTransition {
                from: self.state,
                to: SessionState::Handshake,
            }),
        }
    }
}
