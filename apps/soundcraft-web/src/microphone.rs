use craft_audio::AudioBuffer;
use js_sys::{Array, Float32Array};
use soundcraft_ui_egui::{CaptureEvent, Microphone};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/microphone.js")]
extern "C" {
    type BrowserMicrophone;
    #[wasm_bindgen(constructor)]
    fn new() -> BrowserMicrophone;
    #[wasm_bindgen(method)]
    fn prepare(this: &BrowserMicrophone, rate: u32, frames: u32);
    #[wasm_bindgen(method)]
    fn begin(this: &BrowserMicrophone);
    #[wasm_bindgen(method)]
    fn stop(this: &BrowserMicrophone);
    #[wasm_bindgen(method)]
    fn clear(this: &BrowserMicrophone);
    #[wasm_bindgen(method, getter)]
    fn phase(this: &BrowserMicrophone) -> String;
    #[wasm_bindgen(method, getter)]
    fn message(this: &BrowserMicrophone) -> String;
    #[wasm_bindgen(method, getter)]
    fn chunks(this: &BrowserMicrophone) -> Array;
}

pub struct WebInput {
    device: BrowserMicrophone,
    rate: u32,
}

impl WebInput {
    pub fn new() -> Self {
        Self { device: BrowserMicrophone::new(), rate: 0 }
    }
}

impl Microphone for WebInput {
    fn prepare(&mut self, rate: u32, frames: u32) {
        self.rate = rate;
        self.device.prepare(rate, frames);
    }
    fn poll(&mut self) -> Option<CaptureEvent> {
        match self.device.phase().as_str() {
            "ready" => {
                self.device.begin();
                Some(CaptureEvent::Started)
            }
            "complete" => {
                let warning = self.device.message();
                let chunks = self.device.chunks();
                self.device.clear();
                let mut channels: Vec<Vec<f32>> = Vec::new();
                for chunk in chunks.iter() {
                    let chunk = Array::from(&chunk);
                    if channels.is_empty() {
                        channels.resize_with(chunk.length() as usize, Vec::new);
                    }
                    if chunk.length() == 0 { continue; }
                    for (i, channel) in channels.iter_mut().enumerate() {
                        let plane = Float32Array::new(&chunk.get(i as u32 % chunk.length()));
                        let start = channel.len();
                        channel.resize(start + plane.length() as usize, 0.0);
                        plane.copy_to(&mut channel[start..]);
                    }
                }
                Some(CaptureEvent::Finished(AudioBuffer { sample_rate: self.rate, channels }, warning))
            }
            "failed" => {
                let error = self.device.message();
                self.device.clear();
                Some(CaptureEvent::Failed(error))
            }
            _ => None
        }
    }
    fn stop(&mut self) { self.device.stop(); }
    fn cancel(&mut self) { self.device.clear(); }
}

impl Drop for WebInput {
    fn drop(&mut self) { self.device.clear(); }
}
