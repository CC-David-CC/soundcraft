// This same-origin module runs both in the window and on the audio rendering thread.
// Only the worklet receives live PCM. Its output is silent to avoid microphone feedback.
if (typeof registerProcessor === "function") {
    class TakeProcessor extends AudioWorkletProcessor {
        constructor(options) {
            super();
            this.active = false;
            this.done = false;
            this.limit = options.processorOptions.maxFrames;
            this.frames = 0;
            this.used = 0;
            this.planes = [];
            this.port.onmessage = ({data}) => {
                if (data === "start") this.active = true;
                if (data === "stop") this.finish("");
            };
        }
        flush() {
            if (!this.used) return;
            const chunk = this.planes.map(p => p.subarray(0, this.used));
            this.port.postMessage({chunk}, chunk.map(p => p.buffer));
            this.planes = [];
            this.used = 0;
        }
        finish(warning) {
            if (this.done) return;
            this.done = true;
            this.flush();
            this.port.postMessage({warning});
        }
        process(inputs) {
            if (this.done) return false;
            const input = inputs[0];
            if (!this.active || !input.length) return true;
            const count = Math.min(input.length, 2);
            let offset = 0;
            while (offset < input[0].length && this.frames < this.limit) {
                if (!this.planes.length) this.planes = Array.from({length: count}, () => new Float32Array(4096));
                const n = Math.min(input[0].length - offset, 4096 - this.used, this.limit - this.frames);
                this.planes.forEach((out, i) => out.set(input[i % count].subarray(offset, offset + n), this.used));
                offset += n;
                this.frames += n;
                this.used += n;
                if (this.used === 4096) this.flush();
            }
            if (this.frames === this.limit) this.finish("Take reached the session recording limit.");
            return !this.done;
        }
    }
    registerProcessor("soundcraft-take", TakeProcessor);
}

export class BrowserMicrophone {
    constructor() {
        this.clear();
    }
    clear() {
        this.request = null;
        if (this.stream) this.stream.getTracks().forEach(track => track.stop());
        if (this.node) this.node.disconnect();
        if (this.source) this.source.disconnect();
        if (this.context) this.context.close().catch(() => {});
        this.stream = this.node = this.source = this.context = null;
        this.chunks = [];
        this.phase = "idle";
        this.message = "";
    }
    async prepare(rate, maxFrames) {
        this.clear();
        const request = this.request = {};
        this.phase = "opening";
        try {
            const stream = await navigator.mediaDevices.getUserMedia({audio: {
                echoCancellation: false, noiseSuppression: false, autoGainControl: false,
                sampleRate: {ideal: rate}, channelCount: {ideal: 2}
            }});
            if (this.request !== request) {
                stream.getTracks().forEach(track => track.stop());
                return;
            }
            this.stream = stream;
            const context = this.context = new AudioContext({sampleRate: rate});
            await context.audioWorklet.addModule(import.meta.url);
            if (this.request !== request) return;
            await context.resume();
            if (this.request !== request) return;
            this.node = new AudioWorkletNode(context, "soundcraft-take", {
                channelCount: 2, channelCountMode: "clamped-max", processorOptions: {maxFrames}
            });
            this.node.port.onmessage = ({data}) => {
                if (this.request !== request) return;
                if (data.chunk) this.chunks.push(data.chunk);
                else this.complete(data.warning);
            };
            this.node.onprocessorerror = () => {
                if (this.request === request) this.complete("Audio capture stopped unexpectedly.");
            };
            this.source = context.createMediaStreamSource(stream);
            this.source.connect(this.node);
            this.node.connect(context.destination);
            stream.getAudioTracks()[0].addEventListener("ended", () => {
                if (this.request !== request) return;
                if (this.phase === "ready") {
                    this.clear();
                    this.phase = "failed";
                    this.message = "Microphone disconnected.";
                } else if (this.phase === "recording") {
                    this.message = "Microphone disconnected.";
                    this.stop();
                }
            });
            this.phase = "ready";
        } catch (error) {
            if (this.request !== request) return;
            this.clear();
            this.phase = "failed";
            this.message = error.message || String(error);
        }
    }
    begin() {
        if (this.phase === "ready") {
            this.phase = "recording";
            this.node.port.postMessage("start");
        }
    }
    stop() {
        if (this.phase === "opening" || this.phase === "ready") this.clear();
        if (this.phase !== "recording") return;
        this.phase = "finishing";
        if (this.context.state === "running") this.node.port.postMessage("stop");
        else this.complete("Audio input was interrupted.");
    }
    complete(warning) {
        const chunks = this.chunks;
        const message = warning || this.message;
        this.clear();
        this.chunks = chunks;
        this.message = message;
        this.phase = "complete";
    }
}
