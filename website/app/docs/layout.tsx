import type { Metadata } from "next";
import Chrome from "../chrome";

export const metadata: Metadata = { title: { default: "Docs", template: "%s · Horde docs" } };

export default function DocsLayout({ children }: { children: React.ReactNode }) {
  return <Chrome>{children}</Chrome>;
}
