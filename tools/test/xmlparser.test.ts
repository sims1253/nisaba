import { describe, it, expect } from "vitest";
import { parseXml } from "../src/docx/xmlparser.js";

describe("ordered XML parser", () => {
  it("preserves child order across same-named tags", () => {
    const root = parseXml(`<body><p>a</p><tbl/><p>b</p></body>`);
    expect(root.children[0]!.tag).toBe("body");
    const body = root.children[0]!;
    expect(body.children.map((c) => c.tag)).toEqual(["p", "tbl", "p"]);
    expect(body.children.map((c) => c.text)).toEqual(["a", "", "b"]);
  });

  it("strips namespace prefixes from tags and attributes", () => {
    const root = parseXml(`<w:p xmlns:w="x" w:val="1"><w:t>hi</w:t></w:p>`);
    const p = root.children[0]!;
    expect(p.tag).toBe("p");
    expect(p.attribs["val"]).toBe("1");
    expect(p.children[0]!.tag).toBe("t");
    expect(p.children[0]!.text).toBe("hi");
  });

  it("decodes entities (predefined + numeric)", () => {
    const root = parseXml(`<r><t>&amp;&lt;&gt;&quot;&apos;&#65;&#x42;</t></r>`);
    expect(root.children[0]!.children[0]!.text).toBe(`&<>"'AB`);
  });

  it("handles self-closing tags and attributes", () => {
    const root = parseXml(`<x><y a="1" b='2'/><z/></x>`);
    const x = root.children[0]!;
    expect(x.children[0]!.tag).toBe("y");
    expect(x.children[0]!.attribs).toEqual({ a: "1", b: "2" });
    expect(x.children[1]!.tag).toBe("z");
    expect(x.children[1]!.children).toEqual([]);
  });

  it("handles CDATA and comments", () => {
    const root = parseXml(`<x><!-- c --><![CDATA[a<b>c]]>text</x>`);
    const x = root.children[0]!;
    expect(x.text).toBe("a<b>ctext");
  });

  it("ignores the XML declaration and processing instructions", () => {
    const root = parseXml(`<?xml version="1.0"?><a/>`);
    expect(root.children[0]!.tag).toBe("a");
  });

  it("decodes &lt;&lt;...&gt;&gt; placeholders intact", () => {
    const root = parseXml(`<t>Wirkstoff: &lt;&lt;Wirkstoff&gt;&gt;</t>`);
    expect(root.children[0]!.text).toBe("Wirkstoff: <<Wirkstoff>>");
  });
});
