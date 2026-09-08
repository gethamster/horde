import SiteHeader from "./site-header";

/** Header and article shell shared by the docs and the standalone site pages. */
export default function Chrome({ children }: { children: React.ReactNode }) {
  return (
    <div className="docs">
      <SiteHeader />
      <main><article>{children}</article></main>
    </div>
  );
}
