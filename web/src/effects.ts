/**
 * The PDF boundary between the compile response and the viewer.
 *
 * Compile and export results arrive as base64 in JSON. Preview rendering passes
 * decoded bytes directly to PDF.js; downloads briefly use an object URL.
 */

/** Decodes the service's base64 PDF, rejecting malformed padding or characters. */
export function decodeBase64Pdf(value: string): Uint8Array {
  const normalized = value.replace(/\s/g, "")
  if (normalized.length === 0 || normalized.length % 4 !== 0 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(normalized)) {
    throw new Error("Compile service returned invalid base64 PDF data")
  }
  const binary = atob(normalized)
  return Uint8Array.from(binary, (character) => character.charCodeAt(0))
}

/** Triggers a browser download of base64 content without leaking the object URL. */
export function downloadBase64(base64: string, filename: string, type: string): void {
  const bytes = decodeBase64Pdf(base64)
  const buffer = new ArrayBuffer(bytes.byteLength)
  new Uint8Array(buffer).set(bytes)
  const url = URL.createObjectURL(new Blob([buffer], { type }))
  const link = document.createElement("a")
  link.href = url
  link.download = filename
  link.click()
  // Defer the revoke: some browsers haven't snapshotted the blob data by the
  // time click() returns, so revoking synchronously can abort the download.
  // One event-loop turn is enough for the navigation to consume the blob.
  setTimeout(() => URL.revokeObjectURL(url), 0)
}
