/**
 * Minimal argv parser (`--flag`, `--key value`) plus the shared usage-error
 * type. Option guards return Effects that fail with {@link UsageError} so that
 * usage errors flow through the CLI's failure channel (see `runCli` in
 * render.ts) instead of escaping `dispatch` as synchronous throws.
 */
import { Effect } from "effect";

export interface ParsedArgs {
  readonly command: string | undefined;
  readonly flags: ReadonlySet<string>;
  readonly options: ReadonlyMap<string, string>;
  readonly positional: readonly string[];
}

export function parseArgs(argv: readonly string[]): ParsedArgs {
  const flags = new Set<string>();
  const options = new Map<string, string>();
  const positional: string[] = [];
  let command: string | undefined;
  let i = 0;
  // First non-flag token is the subcommand.
  while (i < argv.length && argv[i]!.startsWith("-")) i++;
  if (i < argv.length) {
    command = argv[i];
    i++;
  }
  for (; i < argv.length; i++) {
    const tok = argv[i]!;
    if (tok.startsWith("--")) {
      const eq = tok.indexOf("=");
      if (eq !== -1) {
        options.set(tok.slice(2, eq), tok.slice(eq + 1));
      } else {
        const key = tok.slice(2);
        const next = argv[i + 1];
        if (next !== undefined && !next.startsWith("-")) {
          options.set(key, next);
          i++;
        } else {
          flags.add(key);
        }
      }
    } else {
      positional.push(tok);
    }
  }
  return { command, flags, options, positional };
}

/**
 * Fetch a required `--key value` option. A missing key fails with a
 * UsageError; a valueless occurrence (parseArgs files a bare `--key` under
 * `flags`) is rejected the same way parseProvenanceOption rejects it, rather
 * than reporting the option as merely missing.
 */
export function requireOption(args: ParsedArgs, key: string): Effect.Effect<string, UsageError> {
  if (args.flags.has(key)) {
    return Effect.fail(new UsageError(`--${key} requires a value`));
  }
  const v = args.options.get(key);
  return v !== undefined
    ? Effect.succeed(v)
    : Effect.fail(new UsageError(`missing required option --${key}`));
}

export class UsageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UsageError";
  }
}
