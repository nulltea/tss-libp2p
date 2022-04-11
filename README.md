# Threshold Signature Scheme over P2P transport 
This project implements [`rust-libp2p`](https://github.com/libp2p/rust-libp2p) transport for {t,n}-threshold signature schemes.

Namely, the protocol used here is [Gennaro, Goldfeder 20 (GG20)](https://eprint.iacr.org/2020/540), which is a full multi-party computation (MPC) algorithm for threshold ECDSA with support for identifying malicious parties. Library that implements it and used here is [`ZenGo-X/multi-party-ecdsa`](https://github.com/ZenGo-X/multi-party-ecdsa).

This codebase aims to follow the modular design of `rust-libp2p`, so could be repurposed to support other elliptic curves and signing schemes, assuming they indented to be run in the [round-based](https://docs.rs/round-based/latest/round_based/index.html) flow of MPC.

## Warning
**Do not use this in production.** Code here hasn't been audited and is likely not stable.
It is no more that a prototype for learning and having fun doing it.

