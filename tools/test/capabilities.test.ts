import { describe, it, expect } from "vitest";
import { Effect, Layer } from "effect";
import { detectCapabilities, TOOL_IDS } from "../src/externals/capabilities.js";
import { Shell, ShellLive } from "../src/externals/shell.js";
import { FileSystem, FileSystemLive } from "../src/externals/fs.js";

const Live = Layer.merge(ShellLive, FileSystemLive);
const run = <A, E>(eff: Effect.Effect<A, E, Shell | FileSystem>) =>
  Effect.runPromise(Effect.provide(eff, Live));

describe("capability detection", () => {
  it("reports every registered tool id", async () => {
    const caps = await run(detectCapabilities());
    for (const id of TOOL_IDS) {
      expect(caps.tools[id]).toBeDefined();
      expect(typeof caps.tools[id].available).toBe("boolean");
    }
  });

  it("derives composite capabilities", async () => {
    const caps = await run(detectCapabilities());
    const keys = ["docxToPdf", "pdfToImage", "imageCompare", "textExtract", "pdfInspect", "indexCheck"] as const;
    for (const key of keys) {
      expect(typeof caps.derived[key]).toBe("boolean");
    }
  });

  it("is deterministic across runs", async () => {
    const a = await run(detectCapabilities());
    const b = await run(detectCapabilities());
    expect(JSON.stringify(a.derived)).toBe(JSON.stringify(b.derived));
  });
});
