//! Input bindings from the `controls` sheet.

use crate::sheets::{ControlsRow, CONTROLS};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

#[derive(SystemParam)]
pub struct Input<'w, 's> {
    pub keys: Res<'w, ButtonInput<KeyCode>>,
    pub pads: Query<'w, 's, &'static Gamepad>,
}

impl Input<'_, '_> {
    fn row(id: usize) -> &'static ControlsRow {
        &CONTROLS[id]
    }

    /// Held this frame (any bound key or pad button).
    pub fn pressed(&self, id: usize) -> bool {
        let r = Self::row(id);
        r.keys.iter().any(|k| self.keys.pressed(*k)) || self.pads.iter().any(|p| r.pad.iter().any(|b| p.pressed(*b)))
    }

    /// Pressed this frame.
    pub fn just_pressed(&self, id: usize) -> bool {
        let r = Self::row(id);
        r.keys.iter().any(|k| self.keys.just_pressed(*k)) || self.pads.iter().any(|p| r.pad.iter().any(|b| p.just_pressed(*b)))
    }

    /// Analog value of the binding's pad buttons (triggers), 0..1.
    pub fn analog(&self, id: usize) -> f32 {
        let r = Self::row(id);
        self.pads.iter().flat_map(|p| r.pad.iter().filter_map(move |b| p.get(*b))).fold(0.0, f32::max)
    }
}

/// `"Up/W: Accelerate"`-style help for every binding in `context`.
pub fn help(context: &str) -> String {
    CONTROLS
        .iter()
        .filter(|r| r.context.as_str() == context)
        .map(|r| {
            let keys: Vec<String> = r.keys.iter().map(|k| format!("{k:?}").replace("Key", "").replace("Arrow", "")).collect();
            format!("{}: {}", keys.join("/"), r.description)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
