pub mod grid;
pub mod timescale;

/// Whether the pointer is over a floating area as drawn this frame. An area placed at a fixed
/// position that egui moved to fit the screen (a menu opened near the bottom) reports
/// `contains_pointer` for where it was asked to go, not where it is (issue 42), so a press on its
/// moved part would look like a click outside it.
pub fn pointer_on_area(ctx: &egui::Context, area: &egui::Response) -> bool {
    ctx.pointer_interact_pos().is_some_and(|p| area.rect.contains(p))
}
