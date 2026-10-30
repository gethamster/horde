import type { ReactNode } from "react";
import { ArrowUp } from "lucide-react";
import { Button } from "@/components/ui/button";
import "./field-shell.css";

export function FieldShell({ children, compact = false, className = "" }: {
  children: ReactNode;
  compact?: boolean;
  className?: string;
}) {
  return <div className={`field-shell${compact ? " field-shell-compact" : ""} ${className}`}>{children}</div>;
}

export function FieldSubmit({ label, disabled = false }: { label: string; disabled?: boolean }) {
  return <Button className="field-submit" size="icon" type="submit" aria-label={label} disabled={disabled}>
    <ArrowUp size={18} aria-hidden="true" />
  </Button>;
}
