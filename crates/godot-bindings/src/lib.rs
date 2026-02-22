//! # Mokosh GDExtension Bindings
//!
//! Godot 4 bindings for the Mokosh networking library.
//!
//! This crate provides Godot-friendly wrappers around the Mokosh client and server,
//! exposing them as GDExtension classes with signals for event-driven gameplay.

mod net_client;
mod net_server;
mod runtime;

use godot::prelude::*;

/// GDExtension entry point - registers all classes with Godot
struct MokoshExtension;

#[gdextension]
unsafe impl ExtensionLibrary for MokoshExtension {}
