//! # Finite State Machine Microframework
//!
//! A lightweight, zero-cost, **event-driven** FSM generator for Rust with **ISR and concurrency support**.
//! Designed for embedded systems (no-std compatible) and high-performance applications.
//!
//! ## Core Features
//!
//! - **Zero Cost:** Compiles to efficient jump tables. No heap allocation, no dynamic dispatch.
//! - **no_std:** Perfect for bare-metal microcontrollers (e.g., STM32, ESP32, nRF).
//! - **ISR-Safe Dispatch:** Thread-safe, lock-free event queue for concurrent environments.
//! - **Declarative Syntax:** Clear, easy-to-read macro for defining states and transitions.
//! - **Thread-Safe Dispatch:** Fully supports concurrent environments (requires `concurrent` feature).

#![no_std]

#[cfg(feature = "std")]
extern crate std;

// The state_machine! macro is automatically available at the crate root
// due to #[macro_export] in fsm.rs
mod fsm;

// Re-export the core types
pub use fsm::Transition;

#[cfg(feature = "diagram")]
pub use diagram_generation::generate_diagram;

#[cfg(feature = "diagram")]
#[doc(hidden)]
pub use fsm::diagram_helpers;

#[cfg(feature = "diagram")]
pub use mermaid_builder;
