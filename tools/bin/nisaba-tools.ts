#!/usr/bin/env bun
/**
 * nisaba-tools CLI entry. Delegates to {@link ../src/cli/index.ts}.
 *
 * Run with: `bun bin/nisaba-tools.ts <command> [options]`
 * (or `npx nisaba-tools` / `bunx nisaba-tools` once linked).
 */
import { main } from "../src/cli/index.js";

await main();
