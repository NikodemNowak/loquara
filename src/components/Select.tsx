import * as RadixSelect from "@radix-ui/react-select";

import { Check, ChevronDown } from "./Icons";

export interface SelectOption {
  value: string;
  label: string;
}

/**
 * A dropdown that Loquara can actually style.
 *
 * The native `<select>` renders its popup with the operating system's own
 * widget, which ignores every token in the app and looks like a different
 * decade next to the rest of the window. This keeps the same keyboard and
 * screen-reader behaviour while drawing the list itself.
 */
export function Select({
  value,
  options,
  onChange,
  disabled,
  label,
  id,
}: {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  /** Accessible name, for the cases where no visible label points at it. */
  label?: string;
  id?: string;
}) {
  const selected = options.find((option) => option.value === value);

  return (
    <RadixSelect.Root value={value} onValueChange={onChange} disabled={disabled}>
      <RadixSelect.Trigger className="select-trigger" aria-label={label} id={id}>
        <RadixSelect.Value>{selected?.label ?? value}</RadixSelect.Value>
        <RadixSelect.Icon className="select-trigger__icon">
          <ChevronDown size={13} />
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal>
        <RadixSelect.Content className="select-menu" position="popper" sideOffset={6}>
          <RadixSelect.Viewport className="select-menu__viewport">
            {options.map((option) => (
              <RadixSelect.Item key={option.value} value={option.value} className="select-item">
                <RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>
                <RadixSelect.ItemIndicator className="select-item__check">
                  <Check size={13} />
                </RadixSelect.ItemIndicator>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  );
}
