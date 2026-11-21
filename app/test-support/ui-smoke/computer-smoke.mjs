// Synthetic RFB 3.8 server in the browser. Run through smoke.mjs.
// Exercises real noVNC rendering/input without starting a desktop or touching host input.
import sharp from 'sharp';

export const desktopImage = sharp({
  create: { width: 1280, height: 800, channels: 3, background: '#31465e' },
}).png().toBuffer();

async function attachVncFixture(screenshot) {
  if (window.top !== window || window.__vncFake) return;
  const image = new Image(); image.src = screenshot; await image.decode();
  const desktop = document.createElement('canvas'); desktop.width = 1280; desktop.height = 800;
  const context = desktop.getContext('2d'); context.drawImage(image, 0, 0, 1280, 800);
  const pixels = context.getImageData(0, 0, 1280, 800).data;
  const Native = window.WebSocket; window.__nativeWebSocket = Native;
  class FakeWebSocket extends EventTarget {
    static CONNECTING = 0; static OPEN = 1; static CLOSING = 2; static CLOSED = 3;
    constructor(url, protocols) {
      super();
      if (!String(url).includes('/computer/vnc?')) return new Native(url, protocols);
      this.url = String(url); this.protocol = Array.isArray(protocols) ? protocols[0] ?? '' : String(protocols ?? ''); this.readyState = 0; this.binaryType = 'arraybuffer';
      this.onopen = null; this.onmessage = null; this.onclose = null; this.onerror = null; this._stage = 0; this._bytes = new Uint8Array();
      window.__vncFake.connections++; window.__vncFake.sockets.push(this);
      setTimeout(() => {
        if (this.readyState !== 0) return;
        this.readyState = 1;
        this._dispatch(new Event('open'));
        if (this.protocol === 'minarbot-vnc-v1') this._emit(new Uint8Array(16 * 1024));
        this._emit(new TextEncoder().encode('RFB 003.008\n'));
      }, 10);
    }
    send(value) {
      const data = value instanceof ArrayBuffer ? new Uint8Array(value) : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
      const merged = new Uint8Array(this._bytes.length + data.length); merged.set(this._bytes); merged.set(data, this._bytes.length); this._bytes = merged;
      while (this._bytes.length) {
        let size = this._stage === 0 ? 12 : this._stage < 3 ? 1 : ({ 0: 20, 3: 10, 4: 8, 5: 6, 6: 8, 255: 12 }[this._bytes[0]] ?? 0);
        if (this._stage === 3 && this._bytes[0] === 2) size = this._bytes.length < 4 ? 0 : 4 + new DataView(this._bytes.buffer).getUint16(2) * 4;
        if (this._stage === 3 && this._bytes[0] === 6) size = this._bytes.length < 8 ? 0 : 8 + Math.abs(new DataView(this._bytes.buffer).getInt32(4));
        if (!size || this._bytes.length < size) return;
        const message = this._bytes.slice(0, size); this._bytes = this._bytes.slice(size);
        if (this._stage === 0) { this._stage = 1; this._emit(new Uint8Array([1, 1])); }
        else if (this._stage === 1) { this._stage = 2; this._emit(new Uint8Array(4)); }
        else if (this._stage === 2) { this._stage = 3; const init = new Uint8Array(24); const view = new DataView(init.buffer); view.setUint16(0, 1280); view.setUint16(2, 800); init[4] = 32; init[5] = 24; init[7] = 1; view.setUint16(8, 255); view.setUint16(10, 255); view.setUint16(12, 255); init[14] = 16; init[15] = 8; this._emit(init); }
        else if (message[0] === 3 && message[1] === 0) {
          const frame = new Uint8Array(16 + 1280 * 800 * 4); const view = new DataView(frame.buffer); view.setUint16(2, 1); view.setUint16(8, 1280); view.setUint16(10, 800);
          for (let i = 16; i < frame.length; i += 4) { frame[i] = pixels[i - 14]; frame[i + 1] = pixels[i - 15]; frame[i + 2] = pixels[i - 16]; frame[i + 3] = 255; }
          window.__vncFake.frames++; this._emit(frame);
        } else if ([4, 5, 6, 255].includes(message[0])) window.__vncFake.input.push([...message]);
      }
    }
    _dispatch(event) {
      const handler = this[`on${event.type}`];
      this.dispatchEvent(event);
      handler?.call(this, event);
    }
    _emit(data) { queueMicrotask(() => { if (this.readyState === 1) this._dispatch(new MessageEvent('message', { data: data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) })); }); }
    close() { if (this.readyState === 3) return; this.readyState = 3; window.__vncFake.sockets = window.__vncFake.sockets.filter(socket => socket !== this); queueMicrotask(() => this._dispatch(new CloseEvent('close', { code: 1000 }))); }
  }
  window.__vncFake = { connections: 0, frames: 0, input: [], sockets: [], closeAll: () => [...window.__vncFake.sockets].forEach(socket => socket.close()) };
  window.WebSocket = FakeWebSocket;
}
export async function installVncFixture(page, image) { await page.evaluate(attachVncFixture, image); }
export async function preloadVncFixture(page, image) { await page.addInitScript(attachVncFixture, image); }
