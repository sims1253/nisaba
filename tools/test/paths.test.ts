import { describe, it, expect } from "vitest";
import { ensureWithin, safeJoin, toPosix, relPosix } from "../src/paths.js";
import { UnsafePathError } from "../src/errors.js";

describe("path helpers", () => {
  it("normalises to POSIX separators", () => {
    // On posix this is a no-op for already-posix paths.
    expect(toPosix("a/b/c")).toBe("a/b/c");
  });

  it("resolve+rel produces POSIX relative paths", () => {
    expect(relPosix("/root/a", "/root/a/b/c.txt")).toBe("b/c.txt");
  });

  it("ensureWithin accepts contained paths", () => {
    expect(ensureWithin("/root", "/root/sub/file.txt")).toBe("/root/sub/file.txt");
    expect(ensureWithin("/root", "/root")).toBe("/root");
  });

  it("ensureWithin rejects escapes", () => {
    expect(() => ensureWithin("/root", "/etc/passwd")).toThrow(UnsafePathError);
    expect(() => ensureWithin("/root", "/root/../etc")).toThrow(UnsafePathError);
  });

  it("safeJoin rejects path-traversal segments", () => {
    expect(() => safeJoin("/root", "../../etc")).toThrow(UnsafePathError);
    expect(safeJoin("/root", "a", "b.txt")).toBe("/root/a/b.txt");
  });
});
