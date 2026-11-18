//! The native SoundCraft side of the WebOS account session.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use js_sys::{Array, ArrayBuffer, Object, Reflect, Uint8Array};
use serde_json::{Value, json};
use soundcraft_engine::io::{ACCOUNT_BUNDLE_BYTES, BUNDLE_EXTENSION, PreparedAccountBundle};
use soundcraft_ui_egui::{AccountSessionFn, AccountSessionRequest, SoundApp};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{MessageEvent, Window};

const PREFIX: &str = "soundcraft:account-";
// The Desktop has one SoundCraft session per iframe generation.
const DOCUMENT: &str = "1";

#[derive(Clone)]
pub(crate) struct Parent {
    window: Window,
    origin: String,
    generation: String,
}

impl Parent {
    pub(crate) fn connect() -> Result<Self, String> {
        let browser = web_sys::window().ok_or("No browser window.")?;
        let window = browser.parent().ok().flatten().ok_or("No parent Desktop.")?;
        let origin = browser.location().origin().map_err(|_| "No browser origin.")?;
        if Object::is(browser.as_ref(), window.as_ref()) || origin == "null" || window.location().origin().ok().as_ref() != Some(&origin) {
            return Err("SoundCraft requires a same-origin WebOS window.".into());
        }
        let mut bytes = [0; 16];
        browser.crypto().and_then(|crypto| crypto.get_random_values_with_u8_array(&mut bytes)).map_err(|_| "Could not identify SoundCraft.")?;
        let generation = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { window, origin, generation })
    }

    pub(crate) fn ready(&self, error: Option<String>) -> Result<(), String> {
        match error {
            Some(message) => self.send("failed", json!({"message":message}), None),
            None => self.send("ready", json!({}), None),
        }
    }

    fn send(&self, kind: &str, mut packet: Value, bytes: Option<&[u8]>) -> Result<(), String> {
        packet["type"] = json!(format!("{PREFIX}{kind}"));
        packet["generation"] = json!(self.generation);
        let packet = js_sys::JSON::parse(&packet.to_string()).map_err(|_| "Could not encode the SoundCraft response.")?;
        let transfer = Array::new();
        if let Some(bytes) = bytes {
            let buffer = Uint8Array::from(bytes).buffer();
            if Reflect::set(&packet, &JsValue::from_str("bytes"), &buffer).ok() != Some(true) {
                return Err("Could not send SoundCraft session bytes.".into());
            }
            transfer.push(&buffer);
        }
        self.window.post_message_with_transfer(&packet, &self.origin, &transfer).map_err(|_| "Could not reach the WebOS Desktop.".into())
    }
}

fn get(packet: &JsValue, name: &str) -> JsValue {
    Reflect::get(packet, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn field(packet: &JsValue, name: &str, limit: usize) -> Option<String> {
    get(packet, name).as_string().filter(|value| value.len() <= limit)
}

fn number(packet: &JsValue, name: &str) -> Option<u64> {
    field(packet, name, 20)?.parse().ok()
}

struct Save {
    ticket: u64,
    revision: u64,
    path: String,
    capture: PreparedAccountBundle,
}

enum DialogAction {
    Navigate,
    Save { revision: u64 },
}

pub(crate) struct AccountHost {
    parent: Parent,
    browser: Window,
    inbox: Rc<RefCell<VecDeque<JsValue>>>,
    listener: Closure<dyn FnMut(MessageEvent)>,
    action: Rc<RefCell<Option<AccountSessionRequest>>>,
    last_state: Value,
    started: bool,
    pending: Option<Save>,
    dialog: Option<(u64, DialogAction)>,
    next_id: u64,
    lease: Option<String>,
    finalized: bool,
}

impl AccountHost {
    pub(crate) fn install(parent: Parent, context: egui::Context) -> Result<Self, String> {
        let browser = web_sys::window().ok_or("No browser window.")?;
        let inbox = Rc::new(RefCell::new(VecDeque::new()));
        let pending = inbox.clone();
        let endpoint = parent.clone();
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if event.origin() != endpoint.origin || !event.source().is_some_and(|source| Object::is(source.as_ref(), endpoint.window.as_ref())) {
                return;
            }
            let packet = event.data();
            if field(&packet, "generation", 32).as_deref() != Some(&endpoint.generation)
                || !field(&packet, "type", 64).is_some_and(|kind| kind.starts_with(PREFIX))
            {
                return;
            }
            if let Ok(mut pending) = pending.try_borrow_mut()
                && pending.len() < 8
            {
                pending.push_back(packet);
                context.request_repaint();
            }
        });
        browser
            .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
            .map_err(|_| "Could not receive WebOS session requests.")?;
        Ok(Self {
            parent,
            browser,
            inbox,
            listener,
            action: Rc::default(),
            last_state: Value::Null,
            started: false,
            pending: None,
            dialog: None,
            next_id: 1,
            lease: None,
            finalized: false,
        })
    }

    pub(crate) fn session_service(&self) -> AccountSessionFn {
        let action = self.action.clone();
        Box::new(move |request| {
            let mut next = action.try_borrow_mut().map_err(|_| "SoundCraft is processing a session request.")?;
            if next.is_some() {
                return Err("SoundCraft is processing a session request.".into());
            }
            *next = Some(request);
            Ok(())
        })
    }

    pub(crate) fn poll(&mut self, app: &mut SoundApp) {
        let packets = std::mem::take(&mut *self.inbox.borrow_mut());
        for packet in packets {
            if let Err(error) = self.receive(app, &packet) {
                app.ui.status = error;
            }
        }
        let action = self.action.borrow_mut().take();
        if let Some(action) = action
            && let Err(error) = self.session_action(app, action)
        {
            app.ui.status = error;
        }
    }

    pub(crate) fn suspended(&self) -> bool {
        !self.started || self.lease.is_some() || self.finalized
    }

    fn unfinished(app: &SoundApp) -> bool {
        app.engine.transport.recording || app.bounce.is_some()
    }

    fn state(&self, app: &SoundApp) -> Value {
        if !self.started {
            return json!({"available":false});
        }
        let revision = app.engine.revision.to_string();
        let unfinished = Self::unfinished(app);
        json!({"available":true,"documentId":DOCUMENT,"revision":revision,"recoveryRevision":revision,
            "dirty":app.engine.is_dirty(),"capturable":!unfinished,"unfinishedEdit":unfinished,"autosaveEligible":false,
            "operationPending":self.pending.is_some() || self.dialog.is_some()})
    }

    pub(crate) fn publish_state(&mut self, app: &SoundApp) -> Result<(), String> {
        let state = self.state(app);
        if self.last_state != state {
            self.parent.send("project-state", state.clone(), None)?;
            self.last_state = state;
        }
        Ok(())
    }

    fn receive(&mut self, app: &mut SoundApp, packet: &JsValue) -> Result<(), String> {
        let kind = field(packet, "type", 64);
        let kind = kind.as_deref().and_then(|kind| kind.strip_prefix(PREFIX));
        if kind == Some("save-result") {
            return self.settle_save(app, packet);
        }
        let Some(id) = field(packet, "requestId", 36) else { return Ok(()) };
        match kind {
            Some("start") => self.start(app, packet, id),
            Some("recovery-capture") => self.capture_recovery(app, packet, id),
            Some("session-save") => self.capture_save(app, packet, id),
            Some("session") => self.session(app, packet, id),
            Some("dialog-result") => self.dialog_result(app, packet),
            Some("operation") => self.operation(app, packet, id),
            _ => Ok(()),
        }
    }

    fn start(&mut self, app: &mut SoundApp, packet: &JsValue, id: String) -> Result<(), String> {
        let result = if self.started { Err("This WebOS window already owns a SoundCraft session.".into()) } else { start_session(app, packet) };
        let reply = match result {
            Ok(()) => {
                self.started = true;
                let mut state = self.state(app);
                state["requestId"] = json!(id);
                state["ok"] = json!(true);
                state["path"] = json!(app.engine.path);
                state
            }
            Err(message) => json!({"requestId":id,"ok":false,"message":message}),
        };
        self.parent.send("started", reply, None)
    }

    fn targets(app: &SoundApp, packet: &JsValue) -> bool {
        field(packet, "documentId", 20).as_deref() == Some(DOCUMENT) && number(packet, "revision") == Some(app.engine.revision)
    }

    fn capture_recovery(&self, app: &SoundApp, packet: &JsValue, id: String) -> Result<(), String> {
        let mut reply = json!({"requestId":id,"documentId":field(packet,"documentId",20),"revision":field(packet,"revision",20)});
        if !self.suspended() && !Self::unfinished(app) && Self::targets(app, packet) {
            match app.engine.capture_account_draft() {
                Ok(bytes) => return self.parent.send("recovery-captured", reply, Some(&bytes)),
                Err(error) => reply["message"] = json!(error.to_string()),
            }
        } else {
            reply["message"] = json!("The SoundCraft session changed before recovery capture.");
        }
        self.parent.send("recovery-captured", reply, None)
    }

    fn operation(&self, app: &mut SoundApp, packet: &JsValue, id: String) -> Result<(), String> {
        let document = field(packet, "documentId", 20).ok_or("Invalid SoundCraft session target.")?;
        let mut reply = json!({"requestId":id,"documentId":document});
        match self.run_operation(app, packet) {
            Ok(value) => reply["result"] = json!(value.to_string()),
            Err(error) => reply["message"] = json!(error),
        }
        self.parent.send("operation-result", reply, None)
    }

    fn run_operation(&self, app: &mut SoundApp, packet: &JsValue) -> Result<Value, String> {
        let path = get(packet, "path");
        let path = if path.is_null() { None } else { Some(path.as_string().ok_or("Invalid SoundCraft session binding.")?) };
        if self.suspended() || field(packet, "documentId", 20).as_deref() != Some(DOCUMENT) || path != app.engine.path {
            return Err("The SoundCraft operation does not match the current session.".into());
        }
        let command = field(packet, "command", 64).ok_or("Invalid SoundCraft operation.")?;
        let args = field(packet, "args", 4096).ok_or("Invalid SoundCraft operation arguments.")?;
        let args: Value = serde_json::from_str(&args).map_err(|_| "Invalid SoundCraft operation arguments.")?;
        let object = args.as_object().ok_or("SoundCraft operation arguments must be an object.")?;
        if command == "document.inspect" && object.is_empty() {
            let session = app.engine.session();
            let tracks: Vec<_> = session.tracks.iter().take(100).map(|track| json!({"name":track.name})).collect();
            return Ok(json!({"name":session.name,"sampleRate":session.sample_rate.hz(),"tracks":tracks,"dirty":app.engine.is_dirty()}));
        }
        if self.pending.is_some() || self.dialog.is_some() || Self::unfinished(app) {
            return Err("Finish the pending SoundCraft edit or operation first.".into());
        }
        match command.as_str() {
            "track.new"
                if object.len() == 1 && object.get("name").and_then(Value::as_str).is_some_and(|name| !name.is_empty() && name.len() <= 128) =>
            {
                app.run("track.new", args)
            }
            "edit.undo" if object.is_empty() => app.run("edit.undo", args),
            _ => Err("Unsupported SoundCraft session operation.".into()),
        }
    }

    fn capture_save(&mut self, app: &mut SoundApp, packet: &JsValue, id: String) -> Result<(), String> {
        let path = field(packet, "path", 2048).ok_or("Invalid SoundCraft destination.")?;
        let revision = field(packet, "revision", 20).ok_or("Invalid SoundCraft save revision.")?;
        let mut reply = json!({"requestId":id,"path":path,"documentId":DOCUMENT,"reviewedRevision":revision});
        if self.pending.is_some() || self.dialog.is_some() || self.suspended() || Self::unfinished(app) || !Self::targets(app, packet) {
            reply["outcome"] = json!("busy");
            return self.parent.send("session-save-result", reply, None);
        }
        if let Err(error) = self.send_save(app, path, "session-save-result", reply.clone()) {
            reply["message"] = json!(error);
            reply["outcome"] = json!("failed");
            self.parent.send("session-save-result", reply, None)?;
        }
        Ok(())
    }

    fn next_request(&mut self) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or("SoundCraft request space exhausted.")?;
        Ok(id)
    }

    fn send_save(&mut self, app: &SoundApp, path: String, kind: &str, mut reply: Value) -> Result<(), String> {
        let revision = app.engine.revision;
        let (capture, bytes) = app.engine.prepare_account_bundle(&path).map_err(|error| error.to_string())?;
        let ticket = self.next_request()?;
        reply["outcome"] = json!("captured");
        reply["ticket"] = json!(ticket.to_string());
        reply["documentId"] = json!(DOCUMENT);
        reply["revision"] = json!(revision.to_string());
        reply["path"] = json!(path);
        self.parent.send(kind, reply, Some(&bytes))?;
        self.pending = Some(Save { ticket, revision, path, capture });
        Ok(())
    }

    fn session_action(&mut self, app: &mut SoundApp, action: AccountSessionRequest) -> Result<(), String> {
        if self.suspended() || self.pending.is_some() || self.dialog.is_some() {
            return Err("Finish the pending SoundCraft session operation first.".into());
        }
        match action {
            AccountSessionRequest::Close => self.parent.send("close-intent", json!({}), None),
            AccountSessionRequest::New(settings) if settings.is_object() => {
                self.request_dialog(json!({"operation":"new","settings":settings.to_string()}), DialogAction::Navigate)
            }
            AccountSessionRequest::New(_) => Err("Invalid SoundCraft session settings.".into()),
            AccountSessionRequest::Open => self.request_dialog(json!({"operation":"open","path":null}), DialogAction::Navigate),
            AccountSessionRequest::Revert => Err("Close and reopen the account session to return to its saved revision.".into()),
            AccountSessionRequest::Save | AccountSessionRequest::SaveAs => {
                if Self::unfinished(app) {
                    return Err("Finish the current recording or bounce before saving.".into());
                }
                if matches!(action, AccountSessionRequest::Save)
                    && let Some(path) = app.engine.path.clone()
                {
                    return self.send_save(app, path, "save", json!({}));
                }
                let suggested = app.engine.path.clone().unwrap_or_else(|| format!("Untitled.{BUNDLE_EXTENSION}"));
                self.request_dialog(json!({"operation":"save","suggested":suggested}), DialogAction::Save { revision: app.engine.revision })
            }
        }
    }

    fn request_dialog(&mut self, mut packet: Value, action: DialogAction) -> Result<(), String> {
        let id = self.next_request()?;
        packet["requestId"] = json!(id.to_string());
        self.parent.send("dialog", packet, None)?;
        self.dialog = Some((id, action));
        Ok(())
    }

    fn dialog_result(&mut self, app: &SoundApp, packet: &JsValue) -> Result<(), String> {
        if self.dialog.as_ref().map(|(id, _)| *id) != number(packet, "requestId") {
            return Ok(());
        }
        let Some((_, action)) = self.dialog.take() else { return Ok(()) };
        match (field(packet, "outcome", 16).as_deref(), action) {
            (Some("failed"), _) => Err(field(packet, "message", 250).unwrap_or_else(|| "The SoundCraft dialog failed.".into())),
            (Some("destination"), DialogAction::Save { revision }) => {
                if revision != app.engine.revision || Self::unfinished(app) || self.suspended() {
                    return Err("The session changed while choosing its save destination.".into());
                }
                let path = field(packet, "path", 2048).ok_or("Invalid SoundCraft destination.")?;
                self.send_save(app, path, "save", json!({}))
            }
            _ => Ok(()),
        }
    }

    fn settle_save(&mut self, app: &mut SoundApp, packet: &JsValue) -> Result<(), String> {
        let matches = self.pending.as_ref().is_some_and(|save| {
            number(packet, "ticket") == Some(save.ticket)
                && field(packet, "documentId", 20).as_deref() == Some(DOCUMENT)
                && number(packet, "revision") == Some(save.revision)
                && field(packet, "path", 2048).as_deref() == Some(save.path.as_str())
        });
        if !matches {
            return Ok(());
        }
        let outcome = field(packet, "outcome", 16);
        if !matches!(outcome.as_deref(), Some("written" | "failed" | "unknown")) {
            return Ok(());
        }
        let Some(save) = self.pending.take() else { return Ok(()) };
        let adopted = outcome.as_deref() == Some("written") && app.engine.adopt_account_bundle(save.capture).is_ok();
        self.parent.send(
            "save-settled",
            json!({"ticket":save.ticket.to_string(),"documentId":DOCUMENT,
                "revision":save.revision.to_string(),"path":save.path,"adopted":adopted,"projectState":self.state(app)}),
            None,
        )
    }

    fn session(&mut self, app: &mut SoundApp, packet: &JsValue, id: String) -> Result<(), String> {
        let operation = field(packet, "operation", 16).ok_or("Invalid SoundCraft session operation.")?;
        let lease = field(packet, "lease", 64).filter(|lease| !lease.is_empty()).ok_or("Invalid SoundCraft session lease.")?;
        let accepted = match operation.as_str() {
            "pause"
                if !self.suspended()
                    && self.pending.is_none()
                    && self.dialog.is_none()
                    && !Self::unfinished(app)
                    && Self::targets(app, packet)
                    && Some(app.engine.is_dirty()) == get(packet, "dirty").as_bool()
                    && get(packet, "unfinishedEdit").as_bool() == Some(false) =>
            {
                app.pause_account_playback();
                self.lease = Some(lease.clone());
                true
            }
            "release" | "finalize" if self.lease.as_deref() == Some(lease.as_str()) => {
                self.lease = None;
                self.finalized = operation == "finalize";
                true
            }
            _ => false,
        };
        self.parent.send(
            "session-result",
            json!({"requestId":id,"operation":operation,"lease":lease,"ok":accepted,
            "message":if accepted {""} else {"The SoundCraft session changed after close review."}}),
            None,
        )
    }
}

impl Drop for AccountHost {
    fn drop(&mut self) {
        let _ = self.browser.remove_event_listener_with_callback("message", self.listener.as_ref().unchecked_ref());
    }
}

fn start_session(app: &mut SoundApp, packet: &JsValue) -> Result<(), String> {
    let path = get(packet, "path");
    let path = if path.is_null() { None } else { Some(path.as_string().ok_or("Invalid account session path.")?) };
    let recover = get(packet, "recover").as_bool().ok_or("Invalid SoundCraft recovery request.")?;
    let bytes = get(packet, "bytes");
    if bytes.is_null() && path.is_none() && !recover {
        let settings = match field(packet, "newSettings", 4096) {
            Some(value) => serde_json::from_str(&value).map_err(|_| "Invalid SoundCraft session settings.")?,
            None => json!({}),
        };
        if !settings.is_object() {
            return Err("Invalid SoundCraft session settings.".into());
        }
        app.engine.execute("session.new", &settings).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let bytes = bytes.dyn_into::<ArrayBuffer>().map_err(|_| "Invalid account session bytes.")?;
    if bytes.byte_length() == 0 || u64::from(bytes.byte_length()) > ACCOUNT_BUNDLE_BYTES {
        return Err("Invalid SoundCraft bundle size.".into());
    }
    app.open_account_session(path.as_deref(), &Uint8Array::new(&bytes).to_vec(), recover)
}
