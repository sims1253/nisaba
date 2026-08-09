/** Tiny dependency-free base64 decoder (for fixture image bytes). */

/** Decode a base64 string to a fresh `Uint8Array`. */
export function base64ToBytes(b64: string): Uint8Array {
  const cleaned = b64.replace(/[^A-Za-z0-9+/=]/g, "");
  const len = Math.floor(cleaned.length * 3) / 4;
  const out = new Uint8Array(len);
  let p = 0;
  for (let i = 0; i < cleaned.length; i += 4) {
    const c0 = B64[cleaned.charCodeAt(i)] ?? 0;
    const c1 = B64[cleaned.charCodeAt(i + 1)] ?? 0;
    const c2 = B64[cleaned.charCodeAt(i + 2)];
    const c3 = B64[cleaned.charCodeAt(i + 3)];
    const n = (c0 << 18) | (c1 << 12) | ((c2 ?? 0) << 6) | (c3 ?? 0);
    if (p < len) out[p++] = (n >> 16) & 0xff;
    if (p < len) out[p++] = (n >> 8) & 0xff;
    if (p < len) out[p++] = n & 0xff;
  }
  return out;
}

const B64 = (() => {
  const arr = new Int16Array(128).fill(-1);
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  for (let i = 0; i < alphabet.length; i++) arr[alphabet.charCodeAt(i)] = i;
  return arr;
})();
