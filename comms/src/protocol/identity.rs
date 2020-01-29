// Copyright 2020, The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
use crate::{
    compat::IoCompat,
    connection::ConnectionDirection,
    message::MessageExt,
    peer_manager::NodeIdentity,
    proto::identity::PeerIdentityMsg,
    protocol::{ProtocolError, ProtocolId, ProtocolNegotiation},
};
use derive_error::Error;
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};
use prost::Message;
use std::{io, sync::Arc};
use tari_utilities::ByteArray;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

const IDENTITY_PROTOCOL: &[u8] = b"/tari/identity/1.0.0";

pub async fn identity_exchange<TSocket>(
    node_identity: Arc<NodeIdentity>,
    direction: ConnectionDirection,
    mut socket: TSocket,
) -> Result<PeerIdentityMsg, IdentityProtocolError>
where
    TSocket: AsyncRead + AsyncWrite + Unpin,
{
    // Negotiate the identity protocol
    let mut negotiation = ProtocolNegotiation::new(&mut socket);
    let proto = match direction {
        ConnectionDirection::Outbound => {
            negotiation
                .negotiate_protocol_outbound(&[ProtocolId::from_static(IDENTITY_PROTOCOL)])
                .await?
        },
        ConnectionDirection::Inbound => {
            negotiation
                .negotiate_protocol_inbound(&[ProtocolId::from_static(IDENTITY_PROTOCOL)])
                .await?
        },
    };

    debug_assert_eq!(proto, IDENTITY_PROTOCOL);

    // Create length-delimited frame codec
    let mut framed = Framed::new(IoCompat::new(socket), LengthDelimitedCodec::new());

    // Send this node's identity
    let msg_bytes = PeerIdentityMsg {
        node_id: node_identity.node_id().to_vec(),
        addresses: vec![node_identity.public_address().to_string()],
        features: node_identity.features().bits(),
    }
    .to_encoded_bytes()
    .map_err(|_| IdentityProtocolError::ProtobufEncodingError)?;

    framed.send(msg_bytes.into()).await?;
    framed.close().await?;

    // Receive the connecting nodes identity
    let msg_bytes = framed
        .next()
        .await
        .ok_or(IdentityProtocolError::PeerUnexpectedCloseConnection)??;
    let identity_msg = PeerIdentityMsg::decode(msg_bytes)?;

    Ok(identity_msg)
}

#[derive(Debug, Error, Clone)]
pub enum IdentityProtocolError {
    #[error(msg_embedded, no_from, non_std)]
    IoError(String),
    #[error(msg_embedded, no_from, non_std)]
    ProtocolError(String),
    #[error(msg_embedded, no_from, non_std)]
    ProtobufDecodeError(String),
    /// Failed to encode protobuf message
    ProtobufEncodingError,
    /// Peer unexpectedly closed the connection
    PeerUnexpectedCloseConnection,
}

impl From<ProtocolError> for IdentityProtocolError {
    fn from(err: ProtocolError) -> Self {
        IdentityProtocolError::ProtocolError(err.to_string())
    }
}

impl From<io::Error> for IdentityProtocolError {
    fn from(err: io::Error) -> Self {
        IdentityProtocolError::IoError(err.to_string())
    }
}

impl From<prost::DecodeError> for IdentityProtocolError {
    fn from(err: prost::DecodeError) -> Self {
        IdentityProtocolError::ProtobufDecodeError(err.to_string())
    }
}

#[cfg(test)]
mod test {
    use crate::{
        connection::ConnectionDirection,
        peer_manager::PeerFeatures,
        test_utils::node_identity::build_node_identity,
        transports::{MemoryTransport, Transport},
    };
    use futures::{future, StreamExt};
    use tari_utilities::ByteArray;

    #[tokio_macros::test_basic]
    async fn identity_exchange() {
        let transport = MemoryTransport;
        let addr = "/memory/0".parse().unwrap();
        let (mut listener, addr) = transport.listen(addr).await.unwrap();

        let (out_sock, in_sock) = future::join(transport.dial(addr), listener.next()).await;

        let out_sock = out_sock.unwrap();
        let in_sock = in_sock.unwrap().map(|(f, _)| f).unwrap().await.unwrap();

        let node_identity1 = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let node_identity2 = build_node_identity(PeerFeatures::COMMUNICATION_CLIENT);

        let (result1, result2) = future::join(
            super::identity_exchange(node_identity1.clone(), ConnectionDirection::Inbound, in_sock),
            super::identity_exchange(node_identity2.clone(), ConnectionDirection::Outbound, out_sock),
        )
        .await;

        // Test node 1 gets node 2's details and vice versa
        let identity2 = result1.unwrap();
        let identity1 = result2.unwrap();

        assert_eq!(identity1.node_id, node_identity1.node_id().to_vec());
        assert_eq!(identity1.features, node_identity1.features().bits());
        assert_eq!(identity1.addresses, vec![node_identity1.public_address().to_string()]);

        assert_eq!(identity2.node_id, node_identity2.node_id().to_vec());
        assert_eq!(identity2.features, node_identity2.features().bits());
        assert_eq!(identity2.addresses, vec![node_identity2.public_address().to_string()]);
    }
}
