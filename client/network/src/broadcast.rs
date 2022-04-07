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
        handler::RequestResponseHandler, ProtocolSupport, RequestResponse, RequestResponseCodec,
        RequestResponseConfig, RequestResponseEvent, RequestResponseMessage, ResponseChannel,
    },
    swarm::{
        protocols_handler::multi::MultiHandler, IntoProtocolsHandler, NetworkBehaviour,
        NetworkBehaviourAction, PollParameters, ProtocolsHandler,
    },
};
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    convert::TryFrom as _,
    io, iter,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub use libp2p::request_response::{InboundFailure, OutboundFailure, RequestId};
use log::error;
use mpc_peerset::{PeersetHandle, ReputationChange};

const BANNED_THRESHOLD: i32 = 82 * (i32::MIN / 100);

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

    /// Channel on which the networking service will send incoming requests.
    ///
    /// Every time a peer sends a request to the local node using this protocol, the networking
    /// service will push an element on this channel. The receiving side of this channel then has
    /// to pull this element, process the request, and send back the response to send back to the
    /// peer.
    ///
    /// The size of the channel has to be carefully chosen. If the channel is full, the networking
    /// service will discard the incoming request send back an error to the peer. Consequently,
    /// the channel being full is an indicator that the node is overloaded.
    ///
    /// You can typically set the size of the channel to `T / d`, where `T` is the
    /// `request_timeout` and `d` is the expected average duration of CPU and I/O it takes to
    /// build a response.
    ///
    /// Can be `None` if the local node does not support answering incoming requests.
    /// If this is `None`, then the local node will not advertise support for this protocol towards
    /// other peers. If this is `Some` but the channel is closed, then the local node will
    /// advertise support for this protocol, but any incoming request will lead to an error being
    /// sent back.
    pub inbound_queue: Option<mpsc::Sender<IncomingMessage>>,
}

impl ProtocolConfig {
    pub fn new(name: Cow<'static, str>, inbound_sender: mpsc::Sender<IncomingMessage>) -> Self {
        Self {
            name,
            max_request_size: 8 * 1024 * 1024,
            max_response_size: 10 * 1024,
            request_timeout: Duration::from_secs(30),
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
    ///
    /// There are two ways to indicate that handling the request failed:
    ///
    /// 1. Drop `pending_response` and thus not changing the reputation of the peer.
    ///
    /// 2. Sending an `Err(())` via `pending_response`, optionally including reputation changes for
    /// the given peer.
    pub pending_response: oneshot::Sender<OutgoingResponse>,
}

/// Response for an incoming request to be send by a request protocol handler.
#[derive(Debug)]
pub struct OutgoingResponse {
    /// The payload of the response.
    ///
    /// `Err(())` if none is available e.g. due an error while handling the request.
    pub result: Result<Vec<u8>, ()>,

    /// Reputation changes accrued while handling the request. To be applied to the reputation of
    /// the peer sending the request.
    pub reputation_changes: Vec<ReputationChange>,

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
        ///
        /// When successful contains the time elapsed between when we received the request and when
        /// we sent back the response. When unsuccessful contains the failure reason.
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

    /// A request protocol handler issued reputation changes for the given peer.
    ReputationChanges {
        peer: PeerId,
        changes: Vec<ReputationChange>,
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
        HashMap<ProtocolRequestId, (Instant, oneshot::Sender<Result<Vec<u8>, RequestFailure>>)>,

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
    message_request: Option<BroadcastMessage>,
}

// This is a state of processing incoming request Message.
// The main reason of this struct is to hold `get_peer_reputation` as a Future state.
struct BroadcastMessage {
    peer: PeerId,
    request_id: RequestId,
    request: GenericPayloadWrapper,
    channel: ResponseChannel<Result<Vec<u8>, ()>>,
    protocol: String,
    resp_builder: Option<futures::channel::mpsc::Sender<IncomingMessage>>,
    // Once we get incoming request we save all params, create an async call to Peerset
    // to get the reputation of the peer.
    get_peer_reputation: Pin<Box<dyn Future<Output = Result<(u16, i32), ()>> + Send>>,
}

/// Generated by the response builder and waiting to be processed.
struct RequestProcessingOutcome {
    peer: PeerId,
    request_id: RequestId,
    protocol: Cow<'static, str>,
    inner_channel: ResponseChannel<Result<Vec<u8>, ()>>,
    response: OutgoingResponse,
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
            message_request: None,
        })
    }

    fn _send_message(
        &mut self,
        target: &PeerId,
        protocol_name: &str,
        request: GenericPayloadWrapper,
        pending_response: oneshot::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        if let Some((protocol, _)) = self.protocols.get_mut(protocol_name) {
            if protocol.is_connected(target) || connect.should_connect() {
                let request_id = protocol.send_request(target, request);
                let prev_req_id = self.pending_requests.insert(
                    (protocol_name.to_string().into(), request_id).into(),
                    (Instant::now(), pending_response),
                );
                debug_assert!(prev_req_id.is_none(), "Expect request id to be unique.");
            } else {
                if pending_response
                    .send(Err(RequestFailure::NotConnected))
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
                .send(Err(RequestFailure::UnknownProtocol))
                .is_err()
            {
                log::debug!(
                    target: "sub-libp2p",
                    "Unknown protocol {:?}. At the same time local \
                     node is no longer interested in the result.",
                    protocol_name,
                );
            };
        }
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
        protocol_name: &str,
        request: Vec<u8>,
        pending_response: oneshot::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        self._send_message(
            target,
            protocol_name,
            GenericPayloadWrapper::Direct(request),
            pending_response,
            connect,
        );
    }

    pub fn broadcast_message<'a>(
        &mut self,
        targets: impl Iterator<Item = &'a PeerId>,
        protocol_name: &str,
        request: Vec<u8>,
        pending_response: oneshot::Sender<Result<Vec<u8>, RequestFailure>>,
        connect: IfDisconnected,
    ) {
        for target in targets {
            let (response_sender, response_receiver) = oneshot::channel();
            self._send_message(
                target,
                protocol_name,
                GenericPayloadWrapper::Broadcast(request.clone()),
                response_sender,
                connect,
            );
        }
        // TODO: aggregate responses for echo broadcast verification
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
            if let Some(message_request) = self.message_request.take() {
                // Now we cannot poll `MessageRequest` until we get the reputation
                let BroadcastMessage {
                    peer,
                    request_id,
                    request,
                    channel,
                    protocol,
                    resp_builder,
                    mut get_peer_reputation,
                } = message_request;

                let reputation = Future::poll(Pin::new(&mut get_peer_reputation), cx);
                match reputation {
                    Poll::Pending => {
                        // Save the state to poll it again next time.
                        self.message_request = Some(BroadcastMessage {
                            peer,
                            request_id,
                            request,
                            channel,
                            protocol,
                            resp_builder,
                            get_peer_reputation,
                        });
                        return Poll::Pending;
                    }
                    Poll::Ready(result) => {
                        // Once we get the reputation we can continue processing the request.

                        let (peer_index, reputation) = result.expect(
                            "Message sender is not a part of our set; qed",
                        );

                        if reputation < BANNED_THRESHOLD {
                            log::debug!(
                                target: "sub-libp2p",
                                "Cannot handle requests from a node with a low reputation {}: {}",
                                peer,
                                reputation,
                            );
                            continue 'poll_all;
                        }

                        let (tx, rx) = oneshot::channel();

                        // Submit the request to the "response builder" passed by the user at
                        // initialization.
                        if let Some(mut resp_builder) = resp_builder {
                            // If the response builder is too busy, silently drop `tx`. This
                            // will be reported by the corresponding `RequestResponse` through
                            // an `InboundFailure::Omission` event.
                            let _ = resp_builder.try_send(IncomingMessage {
                                peer_index,
                                is_broadcast: request.is_broadcast(),
                                payload: request.into_bytes(),
                                pending_response: tx,
                            });
                        } else {
                            debug_assert!(false, "Received message on outbound-only protocol.");
                        }

                        let protocol = Cow::from(protocol);
                        self.pending_responses.push(Box::pin(async move {
                            // The `tx` created above can be dropped if we are not capable of
                            // processing this request, which is reflected as a
                            // `InboundFailure::Omission` event.
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
                    peer,
                    request_id,
                    protocol: protocol_name,
                    inner_channel,
                    response:
                        OutgoingResponse {
                            result,
                            reputation_changes,
                            sent_feedback,
                        },
                } = match outcome {
                    Some(outcome) => outcome,
                    // The response builder was too busy or handling the request failed. This is
                    // later on reported as a `InboundFailure::Omission`.
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

                if !reputation_changes.is_empty() {
                    return Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                        Event::ReputationChanges {
                            peer,
                            changes: reputation_changes,
                        },
                    ));
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

                            let get_peer_reputation =
                                self.peerset.clone().peer_reputation(peer.clone());
                            let get_peer_reputation = Box::pin(get_peer_reputation);

                            // Save the Future-like state with params to poll `get_peer_reputation`
                            // and to continue processing the request once we get the reputation of
                            // the peer.
                            self.message_request = Some(BroadcastMessage {
                                peer,
                                request_id,
                                request,
                                channel,
                                protocol: protocol.to_string(),
                                resp_builder: resp_builder.clone(),
                                get_peer_reputation,
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
                                Some((started, pending_response)) => {
                                    let delivered = pending_response
                                        .send(response.map_err(|()| RequestFailure::Refused))
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
                                Some((started, pending_response)) => {
                                    if pending_response
                                        .send(Err(RequestFailure::Network(error.clone())))
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

pub enum GenericPayloadWrapper {
    Direct(Vec<u8>),
    Broadcast(Vec<u8>),
}

impl GenericPayloadWrapper {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            GenericPayloadWrapper::Direct(bytes) => bytes,
            GenericPayloadWrapper::Broadcast(bytes) => bytes
        }
    }

    fn is_broadcast(&self) -> bool {
        matches!(self, GenericPayloadWrapper::Broadcast(..))
    }

    fn broadcast_marker(&self) -> u8 {
        match self {
            GenericPayloadWrapper::Direct(_) => 0,
            GenericPayloadWrapper::Broadcast(_) => 1
        }
    }
}

/// Implements the libp2p [`RequestResponseCodec`] trait. Defines how streams of bytes are turned
/// into requests and responses and vice-versa.
#[derive(Debug, Clone)]
#[doc(hidden)] // Needs to be public in order to satisfy the Rust compiler.
pub struct GenericCodec {
    max_request_size: u64,
    max_response_size: u64,
}

#[async_trait::async_trait]
impl RequestResponseCodec for GenericCodec {
    type Protocol = Vec<u8>;
    type Request = GenericPayloadWrapper;
    type Response = Result<Vec<u8>, ()>;

    async fn read_request<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let is_broadcast = unsigned_varint::aio::read_u8(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the length.
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        if length > usize::try_from(self.max_request_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Request size exceeds limit: {} > {}",
                    length, self.max_request_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        match is_broadcast {
            0 => Ok(GenericPayloadWrapper::Direct(buffer)),
            1 => Ok(GenericPayloadWrapper::Broadcast(buffer)),
            _ => Ok(GenericPayloadWrapper::Broadcast(buffer)), // todo: proper error handing
        }
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        // Note that this function returns a `Result<Result<...>>`. Returning an `Err` is
        // considered as a protocol error and will result in the entire connection being closed.
        // Returning `Ok(Err(_))` signifies that a response has successfully been fetched, and
        // that this response is an error.

        // Read the length.
        let length = match unsigned_varint::aio::read_usize(&mut io).await {
            Ok(l) => l,
            Err(unsigned_varint::io::ReadError::Io(err))
                if matches!(err.kind(), io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(Err(()))
            }
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidInput, err)),
        };

        if length > usize::try_from(self.max_response_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Response size exceeds limit: {} > {}",
                    length, self.max_response_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;
        Ok(Ok(buffer))
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Write broadcast marker
        {
            let mut buffer = unsigned_varint::encode::u8_buffer();
            io.write_all(unsigned_varint::encode::u8(req.broadcast_marker(), &mut buffer))
                .await?;
        }

        let req = req.into_bytes();

        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(req.len(), &mut buffer))
                .await?;
        }

        // Write the payload.
        io.write_all(&req).await?;

        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // If `res` is an `Err`, we jump to closing the substream without writing anything on it.
        if let Ok(res) = res {
            // TODO: check the length?
            // Write the length.
            {
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(res.len(), &mut buffer))
                    .await?;
            }

            // Write the payload.
            io.write_all(&res).await?;
        }

        io.close().await?;
        Ok(())
    }
}
