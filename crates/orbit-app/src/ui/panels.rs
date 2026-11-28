use super::*;

#[derive(PartialEq, Eq)]
pub(super) enum RightPanel {
    Agents,
    Browser,
    Changes,
    Forks(forks::Panel),
    Settings,
    Worktrees,
}

impl App {
    pub(super) fn discard_settings_edits(&mut self) {
        let draft = if let Some(pending) = &mut self.pending_settings {
            pending.close_on_success = false;
            crate::io_worker::with_live_controls(&pending.submitted, &self.catalog.settings)
        } else {
            self.catalog.settings.clone()
        };
        self.reset_settings_editor(draft);
        self.api_key_draft.clear();
    }

    pub(super) fn toggle_right_panel(&mut self, panel: RightPanel) {
        let next = (self.right_panel.as_ref() != Some(&panel)).then_some(panel);
        self.set_right_panel(next);
    }

    pub(super) fn set_right_panel(&mut self, next: Option<RightPanel>) {
        match self.right_panel {
            Some(RightPanel::Settings) => {
                self.discard_settings_edits();
            }
            Some(RightPanel::Changes) => self.changes.clear(),
            _ => {}
        }
        self.worktrees.popup = None;
        self.right_panel = next;
        self.view_epoch = self.view_epoch.wrapping_add(1);
    }
}
