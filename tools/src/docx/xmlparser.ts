/**
 * A tiny, order-preserving XML parser tuned for OOXML.
 *
 * Why not `fast-xml-parser`? Its default object model groups same-named children
 * under a single key (`{ p: [...], tbl: [...] }`), which destroys the
 * interleaved order of paragraphs and tables in a Word body. A DOCX body's
 * order is semantically meaningful, so we parse into a simple element tree
 * whose `children` array preserves document order exactly.
 *
 * Namespace prefixes (`w:`, `r:`, `wp:` …) are stripped from both tag and
 * attribute names, matching the rest of the code's expectations.
 *
 * This handles the subset real OOXML uses: the XML declaration, comments,
 * CDATA, the five predefined entities plus numeric character references,
 * quoted attributes, and self-closing tags. It is not a general-purpose XML
 * implementation.
 */

export interface XmlElement {
  /** Local tag name (namespace prefix stripped). */
  readonly tag: string;
  /** Attributes keyed by local name. */
  readonly attribs: Record<string, string>;
  /** Direct child elements, in document order. */
  readonly children: XmlElement[];
  /** Concatenated direct text content (entities decoded). */
  readonly text: string;
}

const PREDEF_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
};

function decodeEntities(s: string): string {
  if (!s.includes("&")) return s;
  return s.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[A-Za-z][A-Za-z0-9]*);/g, (m, body: string) => {
    if (body[0] === "#") {
      const isHex = body[1] === "x" || body[1] === "X";
      const code = isHex ? Number.parseInt(body.slice(2), 16) : Number.parseInt(body.slice(1), 10);
      return Number.isFinite(code) ? String.fromCodePoint(code) : m;
    }
    return PREDEF_ENTITIES[body] ?? m;
  });
}

function stripNs(name: string): string {
  const i = name.indexOf(":");
  return i === -1 ? name : name.slice(i + 1);
}

class Parser {
  private readonly s: string;
  private i = 0;
  private readonly root: XmlElement = { tag: "", attribs: {}, children: [], text: "" };
  private readonly stack: XmlElement[] = [this.root];

  constructor(s: string) {
    this.s = s;
  }

  parse(): XmlElement {
    const s = this.s;
    const n = s.length;
    while (this.i < n) {
      const c = s[this.i];
      if (c === "<") {
        this.handleTag();
      } else {
        // text run up to next '<'
        const next = s.indexOf("<", this.i);
        const end = next === -1 ? n : next;
        const chunk = s.slice(this.i, end);
        if (chunk) {
          const top = this.stack[this.stack.length - 1]!;
          (top.text as string) += decodeEntities(chunk);
        }
        this.i = end;
      }
    }
    return this.root;
  }

  private handleTag(): void {
    const s = this.s;
    const n = s.length;
    const start = this.i;
    // classify
    if (s.startsWith("<?", this.i)) {
      const end = s.indexOf("?>", this.i);
      this.i = end === -1 ? n : end + 2;
      return;
    }
    if (s.startsWith("<!--", this.i)) {
      const end = s.indexOf("-->", this.i);
      this.i = end === -1 ? n : end + 3;
      return;
    }
    if (s.startsWith("<![CDATA[", this.i)) {
      const end = s.indexOf("]]>", this.i);
      const contentEnd = end === -1 ? n : end;
      const top = this.stack[this.stack.length - 1]!;
      (top.text as string) += s.slice(this.i + 9, contentEnd);
      this.i = end === -1 ? n : end + 3;
      return;
    }
    if (s[this.i + 1] === "/") {
      // closing tag
      const end = s.indexOf(">", this.i);
      this.i = end === -1 ? n : end + 1;
      if (this.stack.length > 1) this.stack.pop();
      return;
    }
    if (s[this.i + 1] === "!") {
      // <!DOCTYPE ...> or other declaration; skip to matching '>'
      const end = s.indexOf(">", this.i);
      this.i = end === -1 ? n : end + 1;
      return;
    }
    // opening tag: read name
    this.i++; // skip '<'
    let j = this.i;
    while (j < n && !isWS(s[j]) && s[j] !== ">" && s[j] !== "/") j++;
    const rawTag = s.slice(this.i, j);
    this.i = j;
    // read attributes
    const attribs: Record<string, string> = {};
    let selfClosing = false;
    while (this.i < n) {
      // skip whitespace
      while (this.i < n && isWS(s[this.i])) this.i++;
      if (this.i >= n) break;
      const ch = s[this.i];
      if (ch === ">") {
        this.i++;
        break;
      }
      if (ch === "/") {
        selfClosing = true;
        // expect '>' next
        while (this.i < n && s[this.i] !== ">") this.i++;
        if (this.i < n) this.i++;
        break;
      }
      // attribute name
      let k = this.i;
      while (k < n && !isWS(s[k]) && s[k] !== "=" && s[k] !== ">" && s[k] !== "/") k++;
      const aname = stripNs(s.slice(this.i, k));
      this.i = k;
      // skip ws
      while (this.i < n && isWS(s[this.i])) this.i++;
      let aval = "";
      if (s[this.i] === "=") {
        this.i++;
        while (this.i < n && isWS(s[this.i])) this.i++;
        const q = s[this.i];
        if (q === '"' || q === "'") {
          this.i++;
          const close = s.indexOf(q, this.i);
          const ce = close === -1 ? n : close;
          aval = decodeEntities(s.slice(this.i, ce));
          this.i = ce + 1;
        } else {
          // unquoted value (non-spec but tolerate)
          let v = this.i;
          while (v < n && !isWS(s[v]) && s[v] !== ">" && s[v] !== "/") v++;
          aval = decodeEntities(s.slice(this.i, v));
          this.i = v;
        }
      }
      if (aname) attribs[aname] = aval;
    }

    const el: XmlElement = { tag: stripNs(rawTag), attribs, children: [], text: "" };
    this.stack[this.stack.length - 1]!.children.push(el);
    if (!selfClosing) this.stack.push(el);
    void start;
  }
}

function isWS(c: string | undefined): boolean {
  return c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\f";
}

/** Parse an XML document, returning a synthetic root whose `children` hold the document elements. */
export function parseXml(input: string): XmlElement {
  return new Parser(input).parse();
}
