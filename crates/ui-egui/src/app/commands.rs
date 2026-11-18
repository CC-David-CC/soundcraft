use super::{SoundApp, Value, bounce, menus};
use crate::AccountSessionRequest;

impl SoundApp {
    pub(crate) fn routes_account_session(&self, id: &str) -> bool {
        self.services.account_session.is_some()
            && matches!(id, "session.new" | "session.open" | "session.save" | "session.save_as" | "session.close" | "session.revert" | "app.quit")
    }

    /// Run a command; errors go to the status line. Returns the result.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let account_session = self.routes_account_session(id);
        let replaces_session = matches!(id, "session.new" | "session.open" | "session.close" | "session.revert");
        if (replaces_session || (account_session && id == "app.quit")) && bounce::refuses_session_switch(self) {
            return Err(self.ui.status.clone());
        }
        if account_session {
            let action = match id {
                "session.new" => AccountSessionRequest::New(params),
                "session.open" => AccountSessionRequest::Open,
                "session.save" => AccountSessionRequest::Save,
                "session.save_as" => AccountSessionRequest::SaveAs,
                "session.revert" => AccountSessionRequest::Revert,
                "session.close" | "app.quit" => AccountSessionRequest::Close,
                _ => unreachable!("account commands share the session route"),
            };
            if matches!(action, AccountSessionRequest::Save | AccountSessionRequest::SaveAs) {
                self.capture_plugin_states();
            } else if self.engine.transport.recording {
                self.stop_play();
            }
            let result = self.services.account_session.as_mut().ok_or("No account session host.")?(action);
            if let Err(error) = &result {
                self.ui.status = error.clone();
            }
            return result.map(|()| Value::Null);
        }
        if let Some(r) = menus::run_ui_command(self, id, &params) {
            return r;
        }
        if id.starts_with("session.save") || id.starts_with("file.bounce") || id == "track.save_preset" {
            // Third-party plugins keep state the session can't see: store it first.
            self.capture_plugin_states();
        }
        match self.engine.execute(id, &params) {
            Ok(v) => {
                if replaces_session {
                    // Note indices and gestures belong to the old session's clips (#443).
                    self.document_replaced();
                }
                if replaces_session || (id.starts_with("session.save") && !self.engine.is_dirty()) {
                    self.release_autosave(false);
                }
                if id == "app.quit" {
                    self.quit_requested = true;
                }
                if id == "options.scrolling" {
                    // Re-selecting a scrolling mode resumes the playhead follow.
                    self.edit_layout.follow_hold = false;
                }
                if id == "view.zoom_toggle_cancel" && v["canceled"] == true {
                    self.edit_layout.forget_zoom_back();
                }
                if id == "track.clear_clip_indicators" {
                    self.meters.values_mut().chain([&mut self.main_meter]).for_each(|m| m.clip = false);
                }
                Ok(v)
            }
            Err(e) => {
                self.ui.status = e.to_string();
                log::warn!("{id}: {e}");
                Err(e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use crate::{Services, dialogs};
    use serde_json::json;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn account_commands_leave_the_native_session_intact_until_the_host_responds() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let received = actions.clone();
        let services = Services {
            account_session: Some(Box::new(move |action| {
                received.borrow_mut().push(action);
                Ok(())
            })),
            ..Services::default()
        };
        let mut app = SoundApp::new(soundcraft_engine::demo::demo_engine(), None, services);
        app.run("mix.mute", json!({"track": "Kick"})).unwrap();
        menus::invoke_menu(&mut app, "session.open", "File > Open Session...");
        assert!(menus::invoke_shortcut_dialog(&mut app, "session.save"));
        menus::invoke_menu(&mut app, "session.save_as", "File > Save As...");
        menus::invoke_menu(&mut app, "session.revert", "File > Revert");
        menus::invoke_menu(&mut app, "session.close", "File > Close Session");
        let params = json!({"name": "Next Session", "sample_rate": 48_000, "template": "blank"});
        dialogs::run_or_ask(&mut app, "session.new", params.clone()).unwrap();
        assert!(matches!(
            actions.borrow().as_slice(),
            [
                AccountSessionRequest::Open,
                AccountSessionRequest::Save,
                AccountSessionRequest::SaveAs,
                AccountSessionRequest::Revert,
                AccountSessionRequest::Close,
                AccountSessionRequest::New(next),
            ] if next == &params
        ));
        assert!(app.dialogs.open.is_none());
        assert!(app.engine.is_dirty());
        assert!(app.engine.session().track_by_name("Kick").is_some());
    }
}
