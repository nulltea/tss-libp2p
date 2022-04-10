# Threshold Signature Scheme over P2P transport 
This project implements [`rust-libp2p`](https://github.com/libp2p/rust-libp2p) transport for {t,n}-threshold signature schemes.

Namely, the protocol used here is [Gennaro, Goldfeder 20 (GG20)](https://eprint.iacr.org/2020/540), which is a full multi-party computation (MPC) algorithm that supports identifying malicious parties. Library that implements it and used here is [`ZenGo-X/multi-party-ecdsa`](https://github.com/ZenGo-X/multi-party-ecdsa).

This codebase strives to follow modular design of `rust-libp2p`, thus may be repurposed for other elliptic curves, protocols, and signing schemes that are indented to run in round-based flow of MPC.

## Warning
**Do not use this in production.** Code here hasn't been audited and is likely not stable. 
It is no more that a prototype for learning and having fun doing it.
