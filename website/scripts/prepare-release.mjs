import { createPublicKey } from "node:crypto";
import { readFileSync, rmSync } from "node:fs";

const publicDirectory = new URL("../public/", import.meta.url);
const pem = readFileSync(new URL("release-key.pem", publicDirectory), "utf8");
const hex = readFileSync(new URL("release-key.hex", publicDirectory), "utf8").trim();
const key = createPublicKey(pem);
if (key.asymmetricKeyType !== "ed25519" || !/^[0-9a-f]{64}$/.test(hex) ||
    key.export({ type: "spki", format: "der" }).subarray(-32).toString("hex") !== hex) {
  throw new Error("Release public key does not match its pinned Ed25519 fingerprint");
}
// The release owns its installer; website builds must never replace it.
rmSync(new URL("install", publicDirectory), { force: true });
console.log("Verified public key. /install redirects to the latest published release.");
