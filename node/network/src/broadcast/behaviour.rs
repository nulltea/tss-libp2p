// This file was a part of Substrate.
// broadcast.rc <> request_response.rc

// Copyright (C) 2019-2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

use libp2p::{
    core::{
        connection::{ConnectionId, ListenerId},
        ConnectedPoint, Multiaddr, PeerId,
    },
    request_response::{
        handler::RequestResponseHandler, ProtocolSupport, RequestResponse, RequestResponseConfig,
        RequestResponseEvent, RequestResponseMessage, ResponseChannel,
    },
    swarm::{
        protocols_handler::multi::MultiHandler, IntoProtocolsHandler, NetworkBehaviour,
        NetworkBehaviourAction, PollParameters, ProtocolsHandler,
    },
};

use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    io, iter,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use crate::broadcast::generic_codec::{GenericCodec, WireMessage};
pub use libp2p::request_response::{InboundFailure, OutboundFailure, RequestId};
use log::error;
use mpc_peerset::PeersetHandle;

/// Configuration for a single request-response protocol.
#[derive(Debug, Clone)]
pub struct ProtocolConfig {
    /// Name of the protocol on the wire. Should be something like `/foo/bar`.
    pub name: Cow<'static, str>,

    /// Maximum allowed size, in bytes, of a request.
    ///
    /// Any request larger than this value will be declined as a way to avoid allocating too
    /// much memory for it.
    pub max_request_size: u64,

    /// Maximum allowed size, in bytes, of a response.
    ///
    /// Any response larger than this value will be declined as a way to avoid allocating too
    /// much memory for it.
    pub max_response_size: u64,

    /// Duration after which emitted requests are considered timed out.
    ///
    /// If you expect the response to come back quickly, you should set this to a smaller duration.
    pub request_timeout: Duration,

    /// Channel on which the networking service will send incoming messages.
    pub inbound_queue: Option<mpsc::Sender<IncomingMessage>>,
}

impl ProtocolConfig {
    pub fn new(name: Cow<'static, str>, inbound_sender: mpsc::Sender<IncomingMessage>) -> Self {
        Self {
            name,
            max_request_size: 8 * 1024 * 1024,
            max_response_size: 10 * 1024,
            request_timeout: Duration::from_secs(20),
            inbound_queue: Some(inbound_sender),
        }
    }

    pub fn new_with_receiver(
        name: Cow<'static, str>,
        buffer_size: usize,
    ) -> (Self, mpsc::Receiver<IncomingMessage>) {
        let (inbound_sender, inbound_receiver) = mpsc::channel(buffer_size);

        (Self::new(name, inbound_sender), inbound_receiver)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProtoContext {
    pub session_id: u64,
    pub round_index: u16,
}

/// A single request received by a peer on a request-response protocol.
#[derive(Debug)]
pub struct IncomingMessage {
    /// Who sent the request.
    pub peer_index: u16,

    /// Message sent by the remote. Will always be smaller than
    /// [`ProtocolConfig::max_request_size`].
    pub payload: Vec<u8>,

    /// Message send to all peers in peerset.
    pub is_broadcast: bool,

    /// Channel to send back the response.
    pub pending_response: oneshot::Sender<OutgoingResponse>,

    /// Protocol execution context.
    pub context: ProtoContext,
}

/// Response for an incoming request to be send by a request protocol handler.
#[derive(Debug)]
pub struct OutgoingResponse {
    /// The payload of the response.
    ///
    /// `Err(())` if none is available e.g. due an error while handling the request.
    pub result: Result<Vec<u8>, ()>,

    /// If provided, the `oneshot::Sender` will be notified when the request has been sent to the
    /// peer.
    ///
    /// > **Note**: Operating systems typically maintain a buffer of a few dozen kilobytes of
    /// >			outgoing data for each TCP socket, and it is not possible for a user
    /// >			application to inspect this buffer. This channel here is not actually notified
    /// >			when the response has been fully sent out, but rather when it has fully been
    /// >			written to the buffer managed by the operating system.
    pub sent_feedback: Option<oneshot::Sender<()>>,
}

/// Event generated by the [`GenericBroadcast`].
#[derive(Debug)]
pub enum Event {
    /// A remote sent a request and either we have successfully answered it or an error happened.
    ///
    /// This event is generated for statistics purposes.
    InboundMessage {
        /// Peer which has emitted the request.
        peer: PeerId,
        /// Name of the protocol in question.
        protocol: Cow<'static, str>,
        /// Whether handling the request was successful or unsuccessful.
        result: Result<Duration, ResponseFailure>,
    },

    /// A request initiated using [`RequestResponsesBehaviour::send_request`] has succeeded or
    /// failed.
    ///
    /// This event is generated for statistics purposes.
    BroadcastFinished {
        /// Peer that we send a request to.
        peer: PeerId,
        /// Name of the protocol in question.
        protocol: Cow<'static, str>,
        /// Duration the request took.
        duration: Duration,
        /// Result of the request.
        result: Result<(), RequestFailure>,
    },
}

/// Combination of a protocol name and a request id.
///
/// Uniquely identifies an inbound or outbound request among all handled protocols. Note however
/// that uniqueness is only guaranteed between two inbound and likewise between two outbound
/// requests. There is no uniqueness guarantee in a set of both inbound and outbound
/// [`ProtocolRequestId`]s.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProtocolRequestId {
    protocol: Cow<'static, str>,
    request_id: RequestId,
}

impl From<(Cow<'static, str>, RequestId)> for ProtocolRequestId {
    fn from((protocol, request_id): (Cow<'static, str>, RequestId)) -> Self {
        Self {
            protocol,
            request_id,
        }
    }
}

/// When sending a request, what to do on a disconnected recipient.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum IfDisconnected {
    /// Try to connect to the peer.
    TryConnect,
    /// Just fail if the destination is not yet connected.
    ImmediateError,
}

/// Convenience functions for `IfDisconnected`.
impl IfDisconnected {
    /// Shall we connect to a disconnected peer?
    pub fn should_connect(self) -> bool {
        match self {
            Self::TryConnect => true,
            Self::ImmediateError => false,
        }
    }
}

// This is a state of processing incoming request Message.
// The main reason of this struct is to hold `get_peer_reputation` as a Future state.
struct BroadcastMessage {
    peer: PeerId,
    request_id: RequestId,
    request: WireMessage,
    channel: ResponseChannel<Result<Vec<u8>, ()>>,
    protocol: String,
    resp_builder: Option<futures::channel::mpsc::Sender<IncomingMessage>>,
    // Once we get incoming request we save all params, create an async call to Peerset
    // to get the reputation of the peer.
    get_peer_index: Pin<Box<dyn Future<Output = Result<u16, ()>> + Send>>,
}

/// Generated by the response builder and waiting to be processed.
struct RequestProcessingOutcome {
    peer: PeerId,
    request_id: RequestId,
    protocol: Cow<'static, str>,
    inner_channel: ResponseChannel<Result<Vec<u8>, ()>>,
    response: OutgoingResponse,
}

/// Implementation of `NetworkBehaviour` that provides support for request-response protocols.
pub struct GenericBroadcast {
    /// The multiple sub-protocols, by name.
    /// Contains the underlying libp2p `RequestResponse` behaviour, plus an optional
    /// "response builder" used to build responses for incoming requests.
    protocols: HashMap<
        Cow<'static, str>,
        (
            RequestResponse<GenericCodec>,
            Option<mpsc::Sender<IncomingMessage>>,
        ),
    >,

    /// Pending requests, passed down to a [`RequestResponse`] behaviour, awaiting a reply.
    pending_requests:
        HashMap<ProtocolRequestId, (Instant, mpsc::Sender<Result<Vec<u8>, RequestFailure>>)>,

    /// Whenever an incoming request arrives, a `Future` is added to this list and will yield the
    /// start time and the response to send back to the remote.
    pending_responses: stream::FuturesUnordered<
        Pin<Box<dyn Future<Output = Option<RequestProcessingOutcome>> + Send>>,
    >,

    /// Whenever an incoming request arrives, the arrival [`Instant`] is recorded here.
    pending_responses_arrival_time: HashMap<ProtocolRequestId, Instant>,

    /// Whenever a response is received on `pending_responses`, insert a channel to be notified
    /// when the request has been sent out.
    send_feedback: HashMap<ProtocolRequestId, oneshot::Sender<()>>,

    /// Primarily used to get a reputation of a node.
    peerset: PeersetHandle,

    /// Pending message request, holds `MessageRequest` as a Future state to poll it
    /// until we get a response from `Peerset`
    message_wip: Option<BroadcastMessage>,
}

impl GenericBroadcast {
    /// Creates a new behaviour. Must be passed a list of supported protocols. Returns an error if
    /// the same protocol is passed twice.
    pub fn new(
        list: impl Iterator<Item = ProtocolConfig>,
        peerset: PeersetHandle,
    ) -> Result<Self, RegisterError> {
        let mut protocols = HashMap::new();
        for protocol in list {
            let mut cfg = RequestResponseConfig::default();
            cfg.set_connection_keep_alive(Duration::from_secs(20));
            cfg.set_request_timeout(protocol.request_timeout);

            let protocol_support = if protocol.inbound_queue.is_some() {
                ProtocolSupport::Full
            } else {
                ProtocolSupport::Outbound
            };

            let rq_rp = RequestResponse::new(
                GenericCodec {
                    max_request_size: protocol.max_request_size,
                    max_response_size: protocol.max_response_size,
                },
                iter::once((protocol.name.as_bytes().to_vec(), protocol_support)),
                cfg,
            );

            match protocols.entry(protocol.name) {
                Entry::Vacant(e) => e.insert((rq_rp, protocol.inbound_queue)),
                Entry::Occupied(e) => {
                    return Err(RegisterError::DuplicateProtocol(e.key().clone()))
                }
            };
        }

        Ok(Self {
            protocols,
            pending_requests: Default::default(),
            pending_responses: Default::default(),
            pending_responses_arrival_time: Default::default(),
            send_feedback: Default::default(),
            peerset,
            message_wip: None,
        })
    }

    /// Initiates sending a request.
    ///
    /// If there is no established connection to the target peer, the behavior is determined by the
    /// choice of `connect`.
    ///
    /// An error is returned if the protocol doesn't match one that has been registered.
    pub fn send_message(
        &mut self,
        target: &PeerId,
        protocol_id: &str,
        ctx: ProtoContext,
        payload: Vec<u8>,
        pending_response: mpsc::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        self.send_wire_message(
            target,
            protocol_id,
            WireMessage {
                is_broadcast: false,
                payload,
                context: ctx,
            },
            pending_response,
            connect,
        );
    }

    pub fn broadcast_message<'a>(
        &mut self,
        targets: impl Iterator<Item = &'a PeerId>,
        protocol_id: &str,
        ctx: ProtoContext,
        payload: Vec<u8>,
        pending_response: mpsc::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        for target in targets {
            self.send_wire_message(
                target,
                protocol_id,
                WireMessage {
                    is_broadcast: true,
                    payload: payload.clone(),
                    context: ctx,
                },
                pending_response.clone(),
                connect,
            );
        }
    }

    fn send_wire_message(
        &mut self,
        target: &PeerId,
        protocol_id: &str,
        message: WireMessage,
        mut pending_response: mpsc::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        if let Some((protocol, _)) = self.protocols.get_mut(protocol_id) {
            if protocol.is_connected(target) || connect.should_connect() {
                let request_id = protocol.send_request(target, message);
                let prev_req_id = self.pending_requests.insert(
                    (protocol_id.to_string().into(), request_id).into(),
                    (Instant::now(), pending_response),
                );
                debug_assert!(prev_req_id.is_none(), "Expect request id to be unique.");
            } else {
                if pending_response
                    .try_send(Err(RequestFailure::NotConnected))
                    .is_err()
                {
                    log::debug!(
                        target: "sub-libp2p",
                        "Not connected to peer {:?}. At the same time local \
                         node is no longer interested in the result.",
                        target,
                    );
                };
            }
        } else {
            if pending_response
                .try_send(Err(RequestFailure::UnknownProtocol))
                .is_err()
            {
                log::debug!(
                    target: "sub-libp2p",
                    "Unknown protocol {:?}. At the same time local \
                     node is no longer interested in the result.",
                    protocol_id,
                );
            };
        }
    }

    fn new_handler_with_replacement(
        &mut self,
        protocol: String,
        handler: RequestResponseHandler<GenericCodec>,
    ) -> <GenericBroadcast as NetworkBehaviour>::ProtocolsHandler {
        let mut handlers: HashMap<_, _> = self
            .protocols
            .iter_mut()
            .map(|(p, (r, _))| (p.to_string(), NetworkBehaviour::new_handler(r)))
            .collect();

        if let Some(h) = handlers.get_mut(&protocol) {
            *h = handler
        }

        MultiHandler::try_from_iter(handlers).expect(
            "Protocols are in a HashMap and there can be at most one handler per protocol name, \
			 which is the only possible error; qed",
        )
    }
}

impl NetworkBehaviour for GenericBroadcast {
    type ProtocolsHandler =
        MultiHandler<String, <RequestResponse<GenericCodec> as NetworkBehaviour>::ProtocolsHandler>;
    type OutEvent = Event;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        let iter = self
            .protocols
            .iter_mut()
            .map(|(p, (r, _))| (p.to_string(), NetworkBehaviour::new_handler(r)));

        MultiHandler::try_from_iter(iter).expect(
            "Protocols are in a HashMap and there can be at most one handler per protocol name, \
                which is the only possible error; qed",
        )
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_connected(p, peer_id)
        }
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_disconnected(p, peer_id)
        }
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_connection_established(
                p,
                peer_id,
                conn,
                endpoint,
                failed_addresses,
            )
        }
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        _handler: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        for (p, _) in self.protocols.values_mut() {
            let handler = p.new_handler();
            NetworkBehaviour::inject_connection_closed(p, peer_id, conn, endpoint, handler);
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        (p_name, event): <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some((proto, _)) = self.protocols.get_mut(&*p_name) {
            return proto.inject_event(peer_id, connection, event);
        }

        log::warn!(target: "sub-libp2p",
			"inject_node_event: no request-response instance registered for protocol {:?}",
			p_name)
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        _: Self::ProtocolsHandler,
        error: &libp2p::swarm::DialError,
    ) {
        for (p, _) in self.protocols.values_mut() {
            let handler = p.new_handler();
            NetworkBehaviour::inject_dial_failure(p, peer_id, handler, error)
        }
    }

    fn inject_new_listener(&mut self, id: ListenerId) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_new_listener(p, id)
        }
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_new_listen_addr(p, id, addr)
        }
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_expired_listen_addr(p, id, addr)
        }
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_listener_error(p, id, err)
        }
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_listener_closed(p, id, reason)
        }
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_new_external_addr(p, addr)
        }
    }

    fn inject_expired_external_addr(&mut self, addr: &Multiaddr) {
        for (p, _) in self.protocols.values_mut() {
            NetworkBehaviour::inject_expired_external_addr(p, addr)
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ProtocolsHandler>> {
        'poll_all: loop {
            if let Some(message_request) = self.message_wip.take() {
                // Now we cannot pass `BroadcastMessage` until we get the peer_index
                let BroadcastMessage {
                    peer,
                    request_id,
                    request,
                    channel,
                    protocol,
                    resp_builder,
                    mut get_peer_index,
                } = message_request;

                let peer_index = Future::poll(Pin::new(&mut get_peer_index), cx);
                match peer_index {
                    Poll::Pending => {
                        // Save the state to poll it again next time.
                        self.message_wip = Some(BroadcastMessage {
                            peer,
                            request_id,
                            request,
                            channel,
                            protocol,
                            resp_builder,
                            get_peer_index,
                        });
                        return Poll::Pending;
                    }
                    Poll::Ready(result) => {
                        let peer_index =
                            result.expect("Message sender is not a part of our set; qed");

                        let (tx, rx) = oneshot::channel();

                        // Submit the request to the "response builder" passed by the user at
                        // initialization.
                        if let Some(mut resp_builder) = resp_builder {
                            let _ = resp_builder.try_send(IncomingMessage {
                                peer_index,
                                is_broadcast: request.is_broadcast,
                                payload: request.payload,
                                context: request.context,
                                pending_response: tx,
                            });
                        } else {
                            debug_assert!(false, "Received message on outbound-only protocol.");
                        }

                        let protocol = Cow::from(protocol);
                        self.pending_responses.push(Box::pin(async move {
                            if let Ok(response) = rx.await {
                                Some(RequestProcessingOutcome {
                                    peer,
                                    request_id,
                                    protocol,
                                    inner_channel: channel,
                                    response,
                                })
                            } else {
                                None
                            }
                        }));

                        // This `continue` makes sure that `pending_responses` gets polled
                        // after we have added the new element.
                        continue 'poll_all;
                    }
                }
            }
            // Poll to see if any response is ready to be sent back.
            while let Poll::Ready(Some(outcome)) = self.pending_responses.poll_next_unpin(cx) {
                let RequestProcessingOutcome {
                    peer: _,
                    request_id,
                    protocol: protocol_name,
                    inner_channel,
                    response:
                        OutgoingResponse {
                            result,
                            sent_feedback,
                        },
                } = match outcome {
                    Some(outcome) => outcome,
                    None => continue,
                };

                if let Ok(payload) = result {
                    if let Some((protocol, _)) = self.protocols.get_mut(&*protocol_name) {
                        if let Err(_) = protocol.send_response(inner_channel, Ok(payload)) {
                            // Note: Failure is handled further below when receiving
                            // `InboundFailure` event from `RequestResponse` behaviour.
                            log::debug!(
                                target: "sub-libp2p",
                                "Failed to send response for {:?} on protocol {:?} due to a \
                                 timeout or due to the connection to the peer being closed. \
                                 Dropping response",
                                request_id, protocol_name,
                            );
                        } else {
                            if let Some(sent_feedback) = sent_feedback {
                                self.send_feedback
                                    .insert((protocol_name, request_id).into(), sent_feedback);
                            }
                        }
                    }
                }
            }

            // Poll request-responses protocols.
            for (protocol, (behaviour, resp_builder)) in &mut self.protocols {
                while let Poll::Ready(ev) = behaviour.poll(cx, params) {
                    let ev = match ev {
                        // Main events we are interested in.
                        NetworkBehaviourAction::GenerateEvent(ev) => ev,

                        // Other events generated by the underlying behaviour are transparently
                        // passed through.
                        NetworkBehaviourAction::DialAddress { address, handler } => {
                            log::error!(
                                "The request-response isn't supposed to start dialing peers"
                            );
                            let protocol = protocol.to_string();
                            let handler = self.new_handler_with_replacement(protocol, handler);
                            return Poll::Ready(NetworkBehaviourAction::DialAddress {
                                address,
                                handler,
                            });
                        }
                        NetworkBehaviourAction::DialPeer {
                            peer_id,
                            condition,
                            handler,
                        } => {
                            let protocol = protocol.to_string();
                            let handler = self.new_handler_with_replacement(protocol, handler);
                            return Poll::Ready(NetworkBehaviourAction::DialPeer {
                                peer_id,
                                condition,
                                handler,
                            });
                        }
                        NetworkBehaviourAction::NotifyHandler {
                            peer_id,
                            handler,
                            event,
                        } => {
                            return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                                peer_id,
                                handler,
                                event: ((*protocol).to_string(), event),
                            })
                        }
                        NetworkBehaviourAction::ReportObservedAddr { address, score } => {
                            return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr {
                                address,
                                score,
                            })
                        }
                        NetworkBehaviourAction::CloseConnection {
                            peer_id,
                            connection,
                        } => {
                            return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                                peer_id,
                                connection,
                            })
                        }
                    };

                    match ev {
                        // Received a request from a remote.
                        RequestResponseEvent::Message {
                            peer,
                            message:
                                RequestResponseMessage::Request {
                                    request_id,
                                    request,
                                    channel,
                                    ..
                                },
                        } => {
                            self.pending_responses_arrival_time.insert(
                                (protocol.clone(), request_id.clone()).into(),
                                Instant::now(),
                            );

                            let get_peer_index = self.peerset.clone().peer_index(peer.clone());
                            let get_peer_index = Box::pin(get_peer_index);

                            // Save the Future-like state with params to poll `get_peer_index`
                            // and to continue processing the request once we get the index of
                            // the peer.
                            self.message_wip = Some(BroadcastMessage {
                                peer,
                                request_id,
                                request,
                                channel,
                                protocol: protocol.to_string(),
                                resp_builder: resp_builder.clone(),
                                get_peer_index,
                            });

                            // This `continue` makes sure that `message_request` gets polled
                            // after we have added the new element.
                            continue 'poll_all;
                        }

                        // Received a response from a remote to one of our requests.
                        RequestResponseEvent::Message {
                            peer,
                            message:
                                RequestResponseMessage::Response {
                                    request_id,
                                    response,
                                },
                            ..
                        } => {
                            let (started, delivered) = match self
                                .pending_requests
                                .remove(&(protocol.clone(), request_id).into())
                            {
                                Some((started, mut pending_response)) => {
                                    let delivered = pending_response
                                        .try_send(response.map_err(|()| RequestFailure::Refused))
                                        .map_err(|_| RequestFailure::Obsolete);
                                    (started, delivered)
                                }
                                None => {
                                    log::warn!(
                                        target: "sub-libp2p",
                                        "Received `RequestResponseEvent::Message` with unexpected request id {:?}",
                                        request_id,
                                    );
                                    debug_assert!(false);
                                    continue;
                                }
                            };

                            let out = Event::BroadcastFinished {
                                peer,
                                protocol: protocol.clone(),
                                duration: started.elapsed(),
                                result: delivered,
                            };

                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(out));
                        }

                        // One of our requests has failed.
                        RequestResponseEvent::OutboundFailure {
                            peer,
                            request_id,
                            error,
                            ..
                        } => {
                            let started = match self
                                .pending_requests
                                .remove(&(protocol.clone(), request_id).into())
                            {
                                Some((started, mut pending_response)) => {
                                    if pending_response
                                        .try_send(Err(RequestFailure::Network(error.clone())))
                                        .is_err()
                                    {
                                        log::debug!(
                                            target: "sub-libp2p",
                                            "Request with id {:?} failed. At the same time local \
                                             node is no longer interested in the result.",
                                            request_id,
                                        );
                                    }
                                    started
                                }
                                None => {
                                    log::warn!(
                                        target: "sub-libp2p",
                                        "Received `RequestResponseEvent::Message` with unexpected request id {:?}",
                                        request_id,
                                    );
                                    debug_assert!(false);
                                    continue;
                                }
                            };

                            let out = Event::BroadcastFinished {
                                peer,
                                protocol: protocol.clone(),
                                duration: started.elapsed(),
                                result: Err(RequestFailure::Network(error)),
                            };

                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(out));
                        }

                        // An inbound request failed, either while reading the request or due to
                        // failing to send a response.
                        RequestResponseEvent::InboundFailure {
                            request_id,
                            peer,
                            error,
                            ..
                        } => {
                            self.pending_responses_arrival_time
                                .remove(&(protocol.clone(), request_id).into());
                            self.send_feedback
                                .remove(&(protocol.clone(), request_id).into());
                            let out = Event::InboundMessage {
                                peer,
                                protocol: protocol.clone(),
                                result: Err(ResponseFailure::Network(error)),
                            };
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(out));
                        }

                        // A response to an inbound request has been sent.
                        RequestResponseEvent::ResponseSent { request_id, peer } => {
                            let arrival_time = self
                                .pending_responses_arrival_time
                                .remove(&(protocol.clone(), request_id).into())
                                .map(|t| t.elapsed())
                                .expect(
                                    "Time is added for each inbound request on arrival and only \
									 removed on success (`ResponseSent`) or failure \
									 (`InboundFailure`). One can not receive a success event for a \
									 request that either never arrived, or that has previously \
									 failed; qed.",
                                );

                            if let Some(send_feedback) = self
                                .send_feedback
                                .remove(&(protocol.clone(), request_id).into())
                            {
                                let _ = send_feedback.send(());
                            }

                            let out = Event::InboundMessage {
                                peer,
                                protocol: protocol.clone(),
                                result: Ok(arrival_time),
                            };

                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(out));
                        }
                    };
                }
            }

            break Poll::Pending;
        }
    }
}

/// Error when registering a protocol.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// A protocol has been specified multiple times.
    #[error("{0}")]
    DuplicateProtocol(Cow<'static, str>),
}

/// Error in a request.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum RequestFailure {
    #[error("We are not currently connected to the requested peer.")]
    NotConnected,
    #[error("Given protocol hasn't been registered.")]
    UnknownProtocol,
    #[error("Remote has closed the substream before answering, thereby signaling that it considers the request as valid, but refused to answer it.")]
    Refused,
    #[error("The remote replied, but the local node is no longer interested in the response.")]
    Obsolete,
    /// Problem on the network.
    #[error("Problem on the network: {0}")]
    Network(OutboundFailure),
}

/// Error when processing a request sent by a remote.
#[derive(Debug, thiserror::Error)]
pub enum ResponseFailure {
    /// Problem on the network.
    #[error("Problem on the network: {0}")]
    Network(InboundFailure),
}
