# Threshold Signature Scheme over P2P transport
This project implements [`rust-libp2p`](https://github.com/libp2p/rust-libp2p) transport for {t,n}-threshold signature schemes.

Namely, the protocol used here is [Gennaro, Goldfeder 20 (GG20)](https://eprint.iacr.org/2020/540), which is a full multi-party computation (MPC) algorithm for threshold ECDSA with support for identifying malicious parties. Library that implements it and used here is [`ZenGo-X/multi-party-ecdsa`](https://github.com/ZenGo-X/multi-party-ecdsa).

This codebase aims to follow the modular design of `rust-libp2p`, so could be repurposed to support other elliptic curves and signing schemes, assuming they indented to be run in the [round-based](https://docs.rs/round-based/latest/round_based/index.html) flow of MPC.

## Project structure
- `node`: MPC node daemon
  - `network`: libp2p networking stack (broadcast, discovery).
  - `runtime`: engine between network and application layers responsible for orchestrating MPC communication and pre-computation coordination.
  - `rpc`: [`jsonrpc`](https://github.com/paritytech/jsonrpc) server, client, and API trait.
  - `rpc-api`: implements API trait.
- `tss`: application layer implementing two MPC protocols
  - `keygen`: GG20 distributed key generation (DKG)
  - `keysign`: GG20 threshold signing
- `cmd`: helpful CLI for deploying node and interacting with it over JsonRPC.

## Design principles

### The "room" abstraction
The underlying networking protocol is structured around the "room" abstraction. Each room is a separate Req-Resp channel over which parties synchronously run MPC (one at the time). To join the room user have to know its name and at least one other user to bootstrap if Kademlia discovery used.

### Single Proposer; Multiple Joiners
Pre-computation coordination is encoded as session types (see [blog post](https://cathieyun.medium.com/bulletproof-multi-party-computation-in-rust-with-session-types-b3da6e928d5d)) and follows a predefined flow where one party broadcasts a computation proposal and other parties in the room can answer.

Along with the proposal specifying protocol by its id, the Proposer can include an arbitrary challenge serving as means of negotiation (e.g. ask to prove that party has a key share).

After the Proposer sourced enough Joiners, she broadcasts a start message specifying chosen parties and arbitrary arguments relevant for the MPC (e.g. message to be signed).

### Echo broadcast
To ensure reliable broadcast during computation, messages on wire are being echoed, i.e. echo broadcast: once messages from all known parties are received, relayers hash vector containing these messages along with their own ones and send it as an acknowledgment. Assuming relayers sort vectors in the same way (e.g. by party indexes) and all of them received consistent sets of messages, hashes will end up identical and broadcast reliability will be proven.

## Instructions

### Setup a new node
The following command will setup a new node by generating new keypair into path `-p` (pattern :id will be replaced on the peer_id) and create peer config file in path `-c` setting peer address as `-m` and its RPC address as `-r`:
```bash  
cargo run -- setup -c ./peer_config0.json -r 127.0.0.1:8080 -m /ip4/127.0.0.1/tcp/4000 -p ./data/:id/  
```  

### Deploy node
The following command will deploy node with specified config and setup path (resolved by default using `:id` pattern mentioned above). For peer discovery either Kademlia, MDNS, both, or none of these can be used.
```bash  
cargo run -- deploy -c ./config_peer0.json --kademlia  
```  

### Run DKG
The following command will propose to compute a new shared key to `-n` parties in the room `-r` with threshold parameter `-t` using on behalf of a node with the specified RPC address `-a`:
```bash
cargo run -- keygen -a ws://127.0.0.1:8080 -r tss/0 -t 2 -n 3
```

### Run threshold singing
The following command will propose to jointly sign message `-m` to `-t`+1 parties in the room `-r` on behalf of a node with the specified RPC address `-a`:
```bash
cargo run -- sign -a ws://127.0.0.1:8080 -r tss/0 -t 2 -m "hello room!" 
```

## Limitations (future features)
- Rooms needs to be known in advance when deploying node. The future goal is to have them activated inside the node dynamically.
- All rooms source from a single Kad-DHT, which isn't practical. Either DHT-per-room or internal separation needed to be implemented.

## Warning
**Do not use this in production.** Code here hasn't been audited and is likely not stable.  
It is no more that a prototype for learning and having fun doing it.
