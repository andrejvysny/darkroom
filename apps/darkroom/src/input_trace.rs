//! Opt-in input diagnostics (`DARKROOM_TRACE_INPUT=1`): one stderr line per keyboard, modifier,
//! pointer-button and wheel event, first as winit delivered it and then as egui received it.
//! Exists to tell a product input defect from an automation tool that synthesizes events
//! differently from physical hardware (DARKROOM-170/171/172). Cursor motion is logged only while
//! a button is held (to see whether a drag arrives as motion or as a jump).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static BUTTON_DOWN: AtomicBool = AtomicBool::new(false);
use winit::event::WindowEvent;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DARKROOM_TRACE_INPUT").is_some_and(|v| v != "0"))
}

/// The raw window event, before egui-winit translates it.
pub fn winit_event(event: &WindowEvent) {
    if !enabled() {
        return;
    }
    match event {
        WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } => eprintln!(
            "input/winit key state={:?} logical={:?} physical={:?} repeat={} synthetic={is_synthetic}",
            event.state, event.logical_key, event.physical_key, event.repeat
        ),
        WindowEvent::ModifiersChanged(modifiers) => {
            eprintln!("input/winit modifiers {:?}", modifiers.state());
        }
        WindowEvent::MouseInput { state, button, .. } => {
            BUTTON_DOWN.store(state.is_pressed(), Ordering::Relaxed);
            eprintln!("input/winit button {button:?} {state:?}");
        }
        WindowEvent::CursorMoved { position, .. } if BUTTON_DOWN.load(Ordering::Relaxed) => {
            eprintln!("input/winit drag-move ({:.0}, {:.0})", position.x, position.y);
        }
        WindowEvent::MouseWheel { delta, phase, .. } => {
            eprintln!("input/winit wheel {delta:?} phase={phase:?}");
        }
        WindowEvent::PinchGesture { delta, phase, .. } => {
            eprintln!("input/winit pinch {delta} phase={phase:?}");
        }
        WindowEvent::Focused(focused) => eprintln!("input/winit focused={focused}"),
        _ => {}
    }
}

/// The frame's egui input after translation: the events the UI will see (key and pointer events
/// carry their own modifiers; `ModifiersChanged` shows the tracked state).
pub fn egui_input(input: &egui::RawInput) {
    if !enabled() {
        return;
    }
    for event in &input.events {
        if matches!(
            event,
            egui::Event::Key { .. }
                | egui::Event::ModifiersChanged(_)
                | egui::Event::PointerButton { .. }
                | egui::Event::MouseWheel { .. }
                | egui::Event::Zoom(_)
                | egui::Event::Copy
                | egui::Event::Cut
                | egui::Event::Paste(_)
                | egui::Event::Text(_)
        ) {
            eprintln!("input/egui {event:?}");
        }
    }
}
