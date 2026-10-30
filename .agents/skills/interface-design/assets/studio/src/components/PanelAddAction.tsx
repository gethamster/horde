import "./compact-controls.css";
import { Plus, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import "./panel-add-action.css";

/** Shared position and add/cancel treatment for panel creation forms. */
export function PanelAddAction({
  label,
  open,
  onToggle,
  compact = false,
  disabled = false,
}: {
  compact?: boolean;
  disabled?: boolean;
  label: string;
  open: boolean;
  onToggle: () => void;
}) {
  const Icon = open ? X : Plus;
  return (
    <div
      className={`panel-add-action${compact ? " panel-add-action-compact" : " compact-controls"}`}
    >
      <Button
        type="button"
        disabled={disabled}
        size={compact ? "icon-sm" : "sm"}
        variant={compact ? "ghost" : "secondary"}
        aria-label={open ? "Cancel" : label}
        title={compact ? (open ? "Cancel" : label) : undefined}
        aria-expanded={open}
        onClick={onToggle}
      >
        {(!open || compact) && <Icon size={14} aria-hidden="true" />}
        {!compact && (open ? "Cancel" : label)}
      </Button>
    </div>
  );
}
