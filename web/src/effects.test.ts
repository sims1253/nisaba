import { beforeEach, describe, expect, it, vi } from "vitest"
import { decodeBase64Pdf, downloadBase64 } from "./effects"

describe("compile PDF boundary", () => {
  beforeEach(() => {
    vi.stubGlobal("atob", (value: string) => Buffer.from(value, "base64").toString("binary"))
    vi.stubGlobal("URL", { createObjectURL: vi.fn(() => "blob:pdf"), revokeObjectURL: vi.fn() })
  })
  it("decodes base64 into PDF bytes", () => {
    expect([...decodeBase64Pdf("JVBERg==")]).toEqual([37, 80, 68, 70])
  })
  it("rejects malformed base64", () => expect(() => decodeBase64Pdf("not base64!")).toThrow(/invalid base64/i))

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
