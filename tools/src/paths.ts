import path from "node:path";

/** Use forward slashes in report paths. */
export function toPosix(p: string): string {
  return p.split(path.sep).join("/");
}
