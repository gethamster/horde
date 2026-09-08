// The catalog is generated from the Rust tool surface by
// `cargo test --test website_spec`; the site reads it so published counts and
// tool listings cannot drift from what the daemon actually exposes.
import catalog from "../public/.well-known/tools.json";

export const toolCount: number = catalog.count;
export const tools = catalog.tools as ReadonlyArray<{
  name: string;
  description: string;
  scope: "admin" | "worker";
  parameters: { type: string; properties: Record<string, unknown>; required: string[] };
}>;
export const transport = catalog.transport;
export default catalog;
