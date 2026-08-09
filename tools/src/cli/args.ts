/** Minimal, dependency-free argv parser. Supports `--flag` and `--key value`. */
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

export function requireOption(args: ParsedArgs, key: string): string {
  const v = args.options.get(key);
  if (v === undefined) throw new UsageError(`missing required option --${key}`);
  return v;
}

export class UsageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UsageError";
  }
}
