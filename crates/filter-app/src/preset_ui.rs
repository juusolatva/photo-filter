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
mod tests {
    use super::*;

    #[test]
    fn superseded_load_reports_cancellation_without_staging_the_result() {
        let preset = Preset::capture("Old image", &EditState::default(), &Sections::ALL).unwrap();
        let mut panel = PresetUi {
            status: "Loading preset…".into(),
            ..Default::default()
        };
        panel.finish_load(false, Some(Ok(Box::new(preset.clone()))));
        assert!(panel.imported.is_none());
        assert_eq!(panel.apply_sections, Sections::default());
        assert!(panel.status.contains("cancelled"));
        panel.finish_load(true, Some(Ok(Box::new(preset.clone()))));
        assert_eq!(panel.imported, Some(preset));
        assert_eq!(panel.apply_sections, Sections::ALL);
    }

    #[test]
    fn selective_apply_and_capture_preserve_omission_and_explicit_zero() {
        let mut edit = EditState::default();
        edit.calibration.exposure = 2.0;
        edit.tone.contrast = 0.5;
        let sections = Sections {
            calibration: true,
            ..Default::default()
        };
        let preset = capture(&EditState::default(), "Neutral", &sections, None).unwrap();
        let next = prepare_apply(&edit, &preset, &sections).unwrap();
        assert_eq!(next.calibration.exposure, 0.0);
        assert_eq!(next.tone.contrast, 0.5);
        assert_eq!(edit.calibration.exposure, 2.0);
        assert!(preset.patch.tone.is_none());
    }

    #[test]
    fn unsupported_selection_is_transactional_and_can_be_deselected() {
        let mut imported = EditState::default();
        imported.calibration.exposure = 1.0;
        imported.tone.clarity = 0.5;
        let preset = Preset::capture("Future", &imported, &Sections::ALL).unwrap();
        let edit = EditState::default();
        let error = prepare_apply(&edit, &preset, &Sections::ALL).unwrap_err();
        assert!(error.0.contains("Tone"));
        assert_eq!(edit, EditState::default());
        let next = prepare_apply(
            &edit,
            &preset,
            &Sections {
                tone: false,
                ..Sections::ALL
            },
        )
        .unwrap();
        assert_eq!(next.calibration.exposure, 1.0);
        assert_eq!(next.tone.clarity, 0.0);
    }

    #[test]
    fn merge_failures_do_not_advise_deselecting_unsupported_effects() {
        let mut preset = Preset::from_json(include_str!(
            "../../filter-core/tests/fixtures/v1-future-node.pfpreset.json"
        ))
        .unwrap();
        let node = preset.patch.custom_stack.as_ref().unwrap()[0].clone();
        preset.patch.custom_stack = Some(vec![node; filter_core::edit::MAX_STACK_NODES]);
        let mut edit = EditState::default();
        preset.apply_to(&mut edit, &Sections::ALL).unwrap();
        preset.patch.custom_stack_mode = filter_core::preset::StackMode::Append;
        let original = edit.clone();
        let error = prepare_apply(&edit, &preset, &Sections::ALL).unwrap_err();
        assert!(error.0.contains("custom stack exceeds"));
        assert!(!error.0.contains("Deselect"));
        assert_eq!(edit, original);
    }

    #[test]
    fn unknown_operations_and_metadata_survive_app_apply_capture_and_disk() {
        let json = include_str!("../../filter-core/tests/fixtures/v1-future-node.pfpreset.json");
        let mut preset = Preset::from_json(json).unwrap();
        preset.author = "Camera club".into();
        preset.tags = vec!["shared".into(), "future".into()];
        let next = prepare_apply(&EditState::default(), &preset, &Sections::ALL).unwrap();
        let saved = capture(
            &next,
            &preset.name,
            &available(&preset.patch),
            Some(&preset),
        )
        .unwrap();
        assert_ne!(saved.id, preset.id);
        assert_eq!(saved.author, preset.author);
        assert_eq!(saved.tags, preset.tags);
        assert_eq!(
            saved.patch.custom_stack.as_ref().unwrap()[0].op,
            preset.patch.custom_stack.as_ref().unwrap()[0].op
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.pfpreset.json");
        filter_io::preset::write(&path, &saved).unwrap();
        assert_eq!(filter_io::preset::read(&path).unwrap(), saved);
    }

    #[test]
    fn saved_full_preset_restores_the_compiled_render_settings() {
        let mut edit = EditState::default();
        edit.calibration.exposure = 0.75;
        edit.calibration.temperature = -0.25;
        edit.calibration.tint = 0.1;
        edit.tone.brightness = 0.1;
        edit.tone.contrast = 0.2;
        edit.tone.highlights = -0.4;
        edit.tone.shadows = 0.3;
        edit.tone.whites = -0.1;
        edit.tone.blacks = 0.2;
        edit.color.scalars.vibrance = 0.3;
        edit.color.scalars.saturation = -0.2;
        let preset = capture(&edit, "All controls", &Sections::ALL, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all.pfpreset.json");
        filter_io::preset::write(&path, &preset).unwrap();
        let loaded = filter_io::preset::read(&path).unwrap();
        let restored = prepare_apply(&EditState::default(), &loaded, &Sections::ALL).unwrap();
        assert_eq!(restored, edit);
        assert_eq!(
            BasicAdjustments::new(&restored).unwrap().uniform_rows(),
            BasicAdjustments::new(&edit).unwrap().uniform_rows()
        );
    }
}
