// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./format";

// Model output is untrusted. It is rendered as HTML inside the app, where a
// script could call the app's commands, so it must be neutralised.
describe("renderMarkdown sanitises untrusted model output", () => {
  const attacks: [string, string][] = [
    ["script tag", "hi <script>window.pwned=1</script>"],
    ["img onerror", '<img src=x onerror="window.pwned=1">'],
    ["svg onload", '<svg onload="window.pwned=1"></svg>'],
    ["javascript: link", "[click me](javascript:window.pwned=1)"],
    ["iframe", '<iframe src="https://attacker.example"></iframe>'],
    ["inline handler", '<a href="https://ok.example" onclick="window.pwned=1">x</a>'],
    ["style exfil", '<div style="background:url(https://attacker.example/leak)">x</div>'],
    ["form", '<form action="https://attacker.example"><input name=a></form>'],
    ["markdown image (remote fetch)", "![tracker](https://attacker.example/pixel.png)"],
  ];

  for (const [name, payload] of attacks) {
    it(`neutralises ${name}`, () => {
      const html = renderMarkdown(payload).toLowerCase();
      expect(html).not.toContain("<script");
      expect(html).not.toContain("onerror");
      expect(html).not.toContain("onload");
      expect(html).not.toContain("onclick");
      expect(html).not.toContain("javascript:");
      expect(html).not.toContain("<iframe");
      expect(html).not.toContain("<img");
      expect(html).not.toContain("<form");
      expect(html).not.toContain("style=");
      expect(html).not.toContain("<svg");
    });
  }

  it("keeps normal markdown", () => {
    const html = renderMarkdown("**bold** and `code`\n\n```py\nprint(1)\n```\n\n[docs](https://example.com)");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
    expect(html).toContain('href="https://example.com"');
  });
});
