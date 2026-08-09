/**
 * Single source of truth for the tools package version.
 *
 * Keep this in sync with `version` in `package.json`. It is emitted in every
 * machine-readable report so that a manifest or compliance record always records
 * which version of the tooling produced it.
 */
export const VERSION = "0.1.0" as const;

export const TOOL_NAME = "nisaba-tools" as const;
