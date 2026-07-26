//! CrossDesk service crate.
//!
//! Implements the daemon that shares mouse, keyboard and clipboard across
//! machines on a local network: input capture on the sending side
//! ([`capture`]), DTLS transport ([`connect`] outgoing / [`listen`]
//! incoming), input emulation on the receiving side ([`emulation`]) and the
//! [`service`] event loop orchestrating them. Frontends (GUI/CLI) talk to
//! the daemon via `lan-mouse-ipc`; [`runtime`] is the process entry point
//! shared by all binaries.

mod capture;
mod capture_test;
mod client;
mod clipboard;
mod config;
mod connect;
mod crypto;
mod dns;
mod emulation;
mod emulation_test;
mod listen;
mod observability;
pub mod runtime;
mod service;
