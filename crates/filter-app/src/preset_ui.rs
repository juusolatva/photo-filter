use eframe::egui;
use filter_core::{
    cpu_eval::BasicAdjustments,
    edit::{EditState, ModelError},
    preset::{Preset, PresetPatch, Sections},
};

pub enum Action {
    Load,
    Save(Box<Preset>),
    Apply(Box<EditState>),
}

pub struct PresetUi {
    imported: Option<Preset>,
    apply_sections: Sections,
    save_sections: Sections,
    // Retain author/tags when capturing an applied preset under a fresh identity.
    template: Option<Preset>,
    name: String,
    pub status: String,
}
impl Default for PresetUi {
    fn default() -> Self {
        Self {
            imported: None,
            apply_sections: Sections::default(),
            save_sections: Sections::ALL,
            template: None,
            name: "My preset".into(),
            status: String::new(),
        }
    }
}
impl PresetUi {
    pub fn finish_load(&mut self, same_image: bool, result: Option<Result<Box<Preset>, String>>) {
        if !same_image {
            self.status = "Preset load cancelled because another image was requested.".into();
            return;
        }
        match result {
            Some(Ok(preset)) => {
                self.apply_sections = available(&preset.patch);
                self.imported = Some(*preset);
                self.status =
                    "Choose sections, then apply. Unchecked sections stay unchanged.".into();
            }
            Some(Err(error)) => self.status = format!("Could not load preset: {error}"),
            None => self.status = "Preset loading cancelled".into(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, edit: &EditState, enabled: bool) -> Option<Action> {
        let mut action = None;
        egui::CollapsingHeader::new("Presets")
            .default_open(true)
            .show(ui, |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.button("Load preset…").clicked() {
                        action = Some(Action::Load);
                    }
                    if let Some(preset) = &self.imported {
                        ui.label(format!("Loaded: {}", preset.name));
                        section_checkboxes(ui, &mut self.apply_sections, available(&preset.patch));
                        let has_unknown = preset.patch.custom_stack.as_ref().is_some_and(|nodes| {
                            nodes.iter().any(|n| {
                                matches!(n.op, filter_core::pipeline::FilterOp::Unknown(_))
                            })
                        });
                        if has_unknown {
                            ui.label("Unknown filters are preserved and skipped by the renderer.");
                        }
                        if ui
                            .add_enabled(
                                any(self.apply_sections),
                                egui::Button::new("Apply selected sections"),
                            )
                            .clicked()
                        {
                            match prepare_apply(edit, preset, &self.apply_sections) {
                                Ok(next) => {
                                    self.name = preset.name.clone();
                                    self.save_sections = available(&preset.patch);
                                    self.template = Some(preset.clone());
                                    self.status = format!("Applied {}", preset.name);
                                    action = Some(Action::Apply(Box::new(next)));
                                }
                                Err(error) => self.status = format!("Could not apply: {error}"),
                            }
                        }
                        if ui.button("Close loaded preset").clicked() {
                            self.imported = None;
                        }
                        ui.separator();
                    }
                    ui.label("Save current adjustments");
                    ui.text_edit_singleline(&mut self.name);
                    ui.collapsing("Sections to save", |ui| {
                        section_checkboxes(ui, &mut self.save_sections, Sections::ALL);
                    });
                    if ui
                        .add_enabled(
                            !self.name.trim().is_empty() && any(self.save_sections),
                            egui::Button::new("Save preset…"),
                        )
                        .clicked()
                    {
                        match capture(
                            edit,
                            &self.name,
                            &self.save_sections,
                            self.template.as_ref(),
                        ) {
                            Ok(preset) => action = Some(Action::Save(Box::new(preset))),
                            Err(error) => self.status = format!("Could not save: {error}"),
                        }
                    }
                });
                if !self.status.is_empty() {
                    ui.label(&self.status);
                }
            });
        action
    }
}

fn available(patch: &PresetPatch) -> Sections {
    Sections {
        calibration: patch.calibration.is_some(),
        tone: patch.tone.is_some(),
        curves: patch.curves.is_some(),
        color: patch.color.is_some(),
        detail: patch.detail.is_some(),
        finishing: patch.finishing.is_some(),
        custom_stack: patch.custom_stack.is_some(),
    }
}
fn any(sections: Sections) -> bool {
    sections != Sections::default()
}
// Exhaustive destructuring makes new model sections require a UI entry.
fn section_entries(sections: &mut Sections) -> [(&'static str, &mut bool); 7] {
    let Sections {
        calibration,
        tone,
        curves,
        color,
        detail,
        finishing,
        custom_stack,
    } = sections;
    [
        ("Calibration / white balance", calibration),
        ("Tone", tone),
        ("Curves", curves),
        ("Color", color),
        ("Detail", detail),
        ("Finishing", finishing),
        ("Custom stack", custom_stack),
    ]
}
fn section_checkboxes(ui: &mut egui::Ui, sections: &mut Sections, mut enabled: Sections) {
    for ((label, selected), (_, present)) in section_entries(sections)
        .into_iter()
        .zip(section_entries(&mut enabled))
    {
        ui.add_enabled(*present, egui::Checkbox::new(selected, label));
    }
}

fn prepare_apply(
    edit: &EditState,
    preset: &Preset,
    sections: &Sections,
) -> Result<EditState, ModelError> {
    let mut next = edit.clone();
    preset.apply_to(&mut next, sections)?;
    // Keep the currently rendered edit intact if the selected effects cannot run.
    BasicAdjustments::new(&next).map_err(|_| {
        ModelError(format!(
            "selected effects cannot be rendered yet ({}). Deselect unsupported sections and try again",
            unsupported_sections(&next)
        ))
    })?;
    Ok(next)
}

fn unsupported_sections(edit: &EditState) -> String {
    let mut names = Vec::new();
    // Both this diagnostic and the checkboxes use the same exhaustive section list.
    for index in 0..section_entries(&mut Sections::default()).len() {
        let mut sections = Sections::default();
        let entries = section_entries(&mut sections);
        let (name, selected) = entries.into_iter().nth(index).expect("section index");
        *selected = true;
        let mut section = EditState::default();
        if section
            .apply(&PresetPatch::from_edit(edit, &sections), &sections)
            .is_ok()
            && BasicAdjustments::new(&section).is_err()
        {
            names.push(name);
        }
    }
    names.join(", ")
}

fn capture(
    edit: &EditState,
    name: &str,
    sections: &Sections,
    template: Option<&Preset>,
) -> Result<Preset, ModelError> {
    let mut preset = Preset::capture(name.trim(), edit, sections)?;
    if let Some(template) = template {
        preset.author = template.author.clone();
        preset.tags = template.tags.clone();
    }
    Ok(preset)
}

#[cfg(test)]
#[path = "preset_ui_tests.rs"]
mod tests;
