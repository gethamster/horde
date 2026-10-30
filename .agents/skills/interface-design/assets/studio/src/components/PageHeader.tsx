import "./page-header.css";
import type { ReactNode, Ref } from "react";

export function PageHeader({
  title,
  subtitle,
  count,
  actions,
  titleRef,
  leading,
  align = "start",
  children,
}: {
  leading?: ReactNode;
  align?: "start" | "center";
  title: string;
  subtitle?: ReactNode;
  count?: number;
  actions?: ReactNode;
  titleRef?: Ref<HTMLHeadingElement>;
  children?: ReactNode;
}) {
  return (
    <header className="page-header" data-align={align}>
      <div className="page-header-row">
        <div className="page-heading">
          {leading}
          <h1 ref={titleRef} tabIndex={titleRef ? -1 : undefined}>
            {title}
          </h1>
          {count !== undefined && <span className="page-count">{count}</span>}
        </div>
        {actions && <div className="page-actions">{actions}</div>}
      </div>
      {subtitle && <div className="page-subtitle">{subtitle}</div>}
      {children}
    </header>
  );
}
