/**
 * The PDF boundary between the compile response and the viewer.
 *
 * Compile results arrive as base64 in JSON; the viewer needs an object URL. Those URLs
 * are a leak if they are not revoked, hence [`PdfBlobUrlStore`], which owns exactly one
 * live URL at a time.
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

export function createPdfBlobUrl(base64: string): string {
  const bytes = decodeBase64Pdf(base64)
  const buffer = new ArrayBuffer(bytes.byteLength)
  new Uint8Array(buffer).set(bytes)
  return URL.createObjectURL(new Blob([buffer], { type: "application/pdf" }))
}

/** Owns the currently displayed object URL and revokes the one it replaces. */
export class PdfBlobUrlStore {
  private current?: string
  replace(base64: string | undefined): string | undefined {
    // Create the successor before revoking the incumbent. If decoding the new
    // base64 throws (corrupt response from the service) the previous URL stays
    // live and the store remains consistent, instead of holding a dangling,
    // already-revoked handle that a later dispose()/replace() can only no-op.
    const next = base64 === undefined ? undefined : createPdfBlobUrl(base64)
    if (this.current) URL.revokeObjectURL(this.current)
    this.current = next
    return this.current
  }
  dispose(): void {
    if (this.current) URL.revokeObjectURL(this.current)
    this.current = undefined
  }
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
