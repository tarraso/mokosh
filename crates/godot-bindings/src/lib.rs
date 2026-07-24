//! # Mokosh GDExtension Bindings
//!
//! Godot 4 bindings for the Mokosh networking library.
//!
//! This crate provides a Godot-friendly wrapper around the Mokosh client,
//! exposing it as a GDExtension class with signals for event-driven gameplay.

mod net_client;
mod runtime;

use godot::prelude::*;

/// GDExtension entry point - registers all classes with Godot
struct MokoshExtension;

#[gdextension]
unsafe impl ExtensionLibrary for MokoshExtension {}
