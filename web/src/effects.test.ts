import { beforeEach, describe, expect, it, vi } from "vitest"
import { createPdfBlobUrl, decodeBase64Pdf, downloadBase64, PdfBlobUrlStore } from "./effects"

describe("compile PDF boundary", () => {
  beforeEach(() => {
    vi.stubGlobal("atob", (value: string) => Buffer.from(value, "base64").toString("binary"))
    vi.stubGlobal("URL", { createObjectURL: vi.fn(() => "blob:pdf"), revokeObjectURL: vi.fn() })
  })
  it("decodes base64 into PDF bytes and a Blob URL", () => {
    expect([...decodeBase64Pdf("JVBERg==")]).toEqual([37, 80, 68, 70])
    expect(createPdfBlobUrl("JVBERg==")).toBe("blob:pdf")
    expect(URL.createObjectURL).toHaveBeenCalledWith(expect.objectContaining({ type: "application/pdf" }))
  })
  it("rejects malformed base64", () => expect(() => decodeBase64Pdf("not base64!")).toThrow(/invalid base64/i))
  it("revokes the old URL when replacing and on disposal", () => {
    const store = new PdfBlobUrlStore()
    store.replace("JVBERg==")
    store.replace("JVBERg==")
    store.dispose()
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(2)
  })

  it("keeps the previous URL valid when the replacement base64 is malformed", () => {
    // Regression: replace() used to revoke the incumbent before attempting to
    // decode the successor. A thrown decode left the store holding a dangling,
    // already-revoked handle. Now the successor is created first; a throw leaves
    // the previous URL live and the store consistent.
    const store = new PdfBlobUrlStore()
    store.replace("JVBERg==")
    expect(() => store.replace("not base64!")).toThrow(/invalid base64/i)
    expect(URL.revokeObjectURL).not.toHaveBeenCalled()
    store.dispose()
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1)
  })

  it("defers the download object URL revoke so the navigation can consume the blob", () => {
    // jsdom does not implement anchor navigation; stub click so it does not
    // emit a noisy "Not implemented: navigation" warning.
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined)
    vi.useFakeTimers()
    downloadBase64("JVBERg==", "test.pdf", "application/pdf")
    // The revoke must NOT happen synchronously — the browser needs the URL
    // alive while the download navigation initiated by click() is in flight.
    expect(URL.revokeObjectURL).not.toHaveBeenCalled()
    vi.runAllTimers()
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1)
    vi.useRealTimers()
    clickSpy.mockRestore()
  })
})
