// Renders public/og.png before the build so the social card is a real PNG at a
// stable URL. Next's `opengraph-image` file convention emits an extensionless
// route, which static hosts serve without an image content type.
import { createElement as h } from "react";
import { ImageResponse } from "next/og.js";
import { writeFileSync, readFileSync } from "node:fs";

const source = readFileSync(new URL("../app/site.ts", import.meta.url), "utf8");
const field = (name) => new RegExp(`\\b${name}:\\s*"([^"]+)"`).exec(source)?.[1] ?? "";
const tagline = field("tagline");
const install = field("install");

const box = (style, children) => h("div", { style }, children);

const image = new ImageResponse(
  box(
    {
      width: "100%",
      height: "100%",
      display: "flex",
      flexDirection: "column",
      justifyContent: "center",
      background: "#fafafa",
      color: "#171717",
      fontFamily: "monospace",
      padding: "0 96px",
    },
    [
      box({ key: "name", display: "flex", fontSize: 92, fontWeight: 700, letterSpacing: "-0.02em" }, "horde.sh"),
      box({ key: "tagline", display: "flex", fontSize: 40, color: "#525252", marginTop: 20 }, tagline),
      box(
        {
          key: "install",
          display: "flex",
          marginTop: 64,
          padding: "24px 32px",
          border: "1px solid #d4d4d4",
          borderRadius: 12,
          background: "#f3f3f3",
          fontSize: 28,
        },
        install,
      ),
    ],
  ),
  { width: 1200, height: 630 },
);

const png = Buffer.from(await image.arrayBuffer());
if (png.subarray(0, 8).toString("hex") !== "89504e470d0a1a0a") {
  throw new Error("Open Graph image is not a PNG");
}
writeFileSync(new URL("../public/og.png", import.meta.url), png);
console.log(`Rendered public/og.png (${png.length} bytes, 1200x630).`);
