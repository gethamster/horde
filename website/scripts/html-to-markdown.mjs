// Minimal HTML -> Markdown converter for the pages this site actually emits
// (headings, paragraphs, lists, links, inline and fenced code).
//
// The Markdown variants are derived from the built HTML rather than authored
// separately, so the two representations cannot drift.

const entities = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", "#39": "'", nbsp: " ",
  "#x27": "'", "#x2F": "/", mdash: "—", ndash: "–", hellip: "…",
  rsquo: "’", lsquo: "‘", ldquo: "“", rdquo: "”",
};

const decode = (text) =>
  text.replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z]+);/g, (match, name) => {
    if (name in entities) return entities[name];
    if (name[0] === "#") {
      const code = name[1] === "x" || name[1] === "X"
        ? parseInt(name.slice(2), 16)
        : parseInt(name.slice(1), 10);
      return Number.isFinite(code) ? String.fromCodePoint(code) : match;
    }
    return match;
  });

/** Strip tags and collapse whitespace, keeping inline code and emphasis. */
function inline(html) {
  let text = html
    .replace(/<!--.*?-->/gs, "")
    .replace(/<code[^>]*>(.*?)<\/code>/gis, (_, inner) => "`" + decode(inner.replace(/<[^>]+>/g, "")) + "`")
    .replace(/<(strong|b)[^>]*>(.*?)<\/\1>/gis, (_, __, inner) => "**" + inner.replace(/<[^>]+>/g, "") + "**")
    .replace(/<(em|i)[^>]*>(.*?)<\/\1>/gis, (_, __, inner) => "_" + inner.replace(/<[^>]+>/g, "") + "_")
    .replace(/<a\b[^>]*\bhref="([^"]*)"[^>]*>(.*?)<\/a>/gis, (_, href, inner) => {
      const label = decode(inner.replace(/<[^>]+>/g, "")).replace(/\s+/g, " ").trim();
      return label ? `[${label}](${href})` : "";
    })
    .replace(/<br\s*\/?>/gi, " ")
    .replace(/<[^>]+>/g, "");
  return decode(text).replace(/\s+/g, " ").trim();
}

/** Convert one page's `<main>` contents to Markdown. */
export function toMarkdown(html, origin) {
  const main = html.match(/<main\b[^>]*>(.*?)<\/main>/is);
  if (!main) return "";
  // Markdown is read away from the site, so site-relative links must resolve.
  const stripped = main[1].replace(/<!--.*?-->/gs, "");
  const body = origin
    ? stripped.replace(/(<a\b[^>]*\bhref=")(\/[^"]*)"/gi, (_, prefix, path) => `${prefix}${new URL(path, origin)}"`)
    : stripped;
  const blocks = [];
  const pattern =
    /<(h[1-6])\b[^>]*>(.*?)<\/\1>|<pre\b[^>]*>(.*?)<\/pre>|<(ul|ol)\b[^>]*>(.*?)<\/\4>|<p\b[^>]*>(.*?)<\/p>/gis;
  for (const match of body.matchAll(pattern)) {
    const [, heading, headingText, preText, listTag, listText, paragraph] = match;
    if (heading) {
      const text = inline(headingText);
      if (text) blocks.push("#".repeat(Number(heading[1])) + " " + text);
    } else if (preText !== undefined) {
      const code = decode(preText.replace(/<[^>]+>/g, ""));
      blocks.push("```sh\n" + code.replace(/\s+$/, "") + "\n```");
    } else if (listText !== undefined) {
      const items = [...listText.matchAll(/<li\b[^>]*>(.*?)<\/li>/gis)]
        .map(([, item], index) => {
          const text = inline(item);
          return text ? (listTag === "ol" ? `${index + 1}. ${text}` : `- ${text}`) : "";
        })
        .filter(Boolean);
      if (items.length) blocks.push(items.join("\n"));
    } else if (paragraph !== undefined) {
      const text = inline(paragraph);
      if (text) blocks.push(text);
    }
  }
  return blocks.join("\n\n").replace(/\n{3,}/g, "\n\n").trim();
}

export { inline as inlineText, decode };
