import Link from "next/link";

/** Brand and primary navigation, on every page including the homepage. */
export default function SiteHeader() {
  return (
    <header className="site-header">
      <Link className="brand" href="/">horde.sh</Link>
      <nav aria-label="Site">
        <Link href="/docs">Set up</Link>
        <Link href="/docs/agents">Agents</Link>
        <Link href="/docs/configuration">Configure</Link>
        <Link href="/docs/deployment">Deploy</Link>
        <Link href="/about">About</Link>
      </nav>
    </header>
  );
}
