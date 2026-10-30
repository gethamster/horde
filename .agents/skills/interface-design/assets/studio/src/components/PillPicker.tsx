import type { RefObject } from "react";
import { Select } from "@base-ui/react/select";
import { Check, ChevronDown } from "lucide-react";
import { Button } from "@/components/ui/button";
import "./pill-picker.css";

export function PillPicker({ value, onChange, options, ariaLabel, disabled = false, autoFocus, portalContainer }: {
  value: string;
  onChange: (value: string) => void;
  options: { value: string; label: string }[];
  ariaLabel: string;
  disabled?: boolean;
  autoFocus?: boolean;
  portalContainer?: HTMLElement | RefObject<HTMLElement | null> | null;
}) {
  return (
    <Select.Root value={value} items={options} disabled={disabled}
      onValueChange={(next) => { if (next !== null) onChange(next); }}>
      <Select.Trigger className="pill-picker-trigger" aria-label={ariaLabel} autoFocus={autoFocus}
        render={<Button size="sm" variant="secondary" />}>
        <Select.Value />
        <Select.Icon><ChevronDown size={14} /></Select.Icon>
      </Select.Trigger>
      <Select.Portal container={portalContainer}>
        <Select.Positioner alignItemWithTrigger align="end" className="pill-picker-positioner">
          <Select.Popup className="pill-picker-popup" aria-label={ariaLabel}>
            <Select.List className="pill-picker-list">
              {options.map((option) => (
                <Select.Item key={option.value} value={option.value} className="pill-picker-option">
                  <Select.ItemText>{option.label}</Select.ItemText>
                  <Select.ItemIndicator><Check size={14} /></Select.ItemIndicator>
                </Select.Item>
              ))}
            </Select.List>
          </Select.Popup>
        </Select.Positioner>
      </Select.Portal>
    </Select.Root>
  );
}
