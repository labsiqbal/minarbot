// Minarbot component kit: Radix primitives for behavior, Bot HQ tokens for the look (see kit.css).
import { createContext, forwardRef, useCallback, useContext, useEffect, useId, useLayoutEffect, useRef, useState, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type Ref, type TextareaHTMLAttributes } from 'react';
import { AlertDialog, Checkbox as RadixCheckbox, Collapsible as RadixCollapsible, Dialog as RadixDialog, DropdownMenu, Popover, Select as RadixSelect, Switch as RadixSwitch, Tabs as RadixTabs, Tooltip as RadixTooltip } from 'radix-ui';
import { AlertTriangle, Check, ChevronDown, ChevronRight, CircleAlert, Info, X } from 'lucide-react';
import { getNonce } from 'get-nonce';
import { useDelayedVisible } from '../hooks/useDelayedVisible';

const cx = (...names: (string | false | null | undefined)[]) => names.filter(Boolean).join(' ');

// ---------- buttons ----------
type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'primary' | 'secondary' | 'ghost' | 'danger' | 'brand'; size?: 'sm' | 'md'; busy?: boolean; icon?: ReactNode };
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button({ variant = 'secondary', size = 'md', busy, icon, className, children, disabled, type = 'button', ...rest }, ref) {
  return <button ref={ref} type={type} className={cx('k-btn', `k-btn--${variant}`, `k-btn--${size}`, busy && 'is-busy', className)} disabled={disabled || busy} aria-busy={busy || undefined} {...rest}>
    {busy ? <span className="k-spinner" aria-hidden /> : icon}{children}
  </button>;
});
export const IconButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { label: string; tooltip?: string }>(function IconButton({ label, tooltip = label, className, children, type = 'button', ...rest }, ref) {
  return <Tooltip content={tooltip}><button ref={ref} type={type} className={cx('k-icon-btn', className)} aria-label={label} {...rest}>{children}</button></Tooltip>;
});

// ---------- fields ----------
type FieldContext = { id: string; hintId: string; errorId: string; describedBy?: string; invalid?: boolean };
const FieldCtx = createContext<FieldContext | null>(null);
export function useField() { return useContext(FieldCtx); }
export function Field({ label, hint, error, children, optional }: { label: ReactNode; hint?: ReactNode; error?: ReactNode; children: ReactNode; optional?: boolean }) {
  const base = useId(), id = `${base}-input`, hintId = `${base}-hint`, errorId = `${base}-error`;
  const describedBy = [hint && hintId, error && errorId].filter(Boolean).join(' ') || undefined;
  return <FieldCtx.Provider value={{ id, hintId, errorId, describedBy, invalid: !!error }}>
    <div className={cx('k-field', !!error && 'is-invalid')}>
      <label className="k-field__label" htmlFor={id}>{label}{optional && <span className="k-field__optional">Optional</span>}</label>
      {children}
      {hint && <div id={hintId} className="k-field__hint">{hint}</div>}
      {error && <div id={errorId} className="k-field__error" role="alert">{error}</div>}
    </div>
  </FieldCtx.Provider>;
}
export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input({ className, id, ...rest }, ref) {
  const field = useField();
  return <input ref={ref} id={id ?? field?.id} aria-describedby={field?.describedBy} aria-invalid={field?.invalid || undefined} className={cx('k-input', className)} {...rest} />;
});
export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(function Textarea({ className, id, ...rest }, ref) {
  const field = useField();
  return <textarea ref={ref} id={id ?? field?.id} aria-describedby={field?.describedBy} aria-invalid={field?.invalid || undefined} className={cx('k-input', 'k-textarea', className)} {...rest} />;
});

export type SelectOption = { value: string; label: ReactNode; hint?: ReactNode; disabled?: boolean };
/** Radix Select. Empty-string values are mapped internally because Radix reserves them for "no selection". */
export function Select({ value, onChange, options, disabled, placeholder, valueLabel, name, label, size = 'md' }: { value: string; onChange: (value: string) => void; options: SelectOption[]; disabled?: boolean; placeholder?: string; valueLabel?: ReactNode; name?: string; label?: string; size?: 'sm' | 'md' }) {
  const field = useField();
  const encode = (v: string) => v === '' ? '\u0000' : v, decode = (v: string) => v === '\u0000' ? '' : v;
  return <RadixSelect.Root value={encode(value)} onValueChange={next => { if (next) onChange(decode(next)); }} disabled={disabled} name={name}>
    <RadixSelect.Trigger id={field?.id} aria-describedby={field?.describedBy} aria-invalid={field?.invalid || undefined} aria-label={label} className={cx('k-input', 'k-select', size === 'sm' && 'k-select--sm')}>
      <RadixSelect.Value placeholder={placeholder}>{valueLabel}</RadixSelect.Value>
      <RadixSelect.Icon className="k-select__chev"><ChevronDown size={14} aria-hidden /></RadixSelect.Icon>
    </RadixSelect.Trigger>
    <RadixSelect.Portal>
      <RadixSelect.Content className="k-pop k-select__content" position="popper" sideOffset={6}>
        <RadixSelect.Viewport className="k-select__viewport" nonce={getNonce()}>
          {options.map(option => <RadixSelect.Item key={option.value} value={encode(option.value)} disabled={option.disabled} className="k-menu__item k-select__item">
            <span className="k-select__text"><RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>{option.hint && <span className="k-menu__hint">{option.hint}</span>}</span>
            <RadixSelect.ItemIndicator className="k-select__check"><Check size={14} aria-hidden /></RadixSelect.ItemIndicator>
          </RadixSelect.Item>)}
        </RadixSelect.Viewport>
      </RadixSelect.Content>
    </RadixSelect.Portal>
  </RadixSelect.Root>;
}

export type ComboboxSuggestion = string | { value: string; label: string };
/** Text input with a suggestion popover; the value stays free text. */
export function Combobox({ value, onChange, suggestions, name, placeholder, readOnly, required, maxLength, emptyText }: { value: string; onChange: (value: string) => void; suggestions: ComboboxSuggestion[]; name?: string; placeholder?: string; readOnly?: boolean; required?: boolean; maxLength?: number; emptyText?: string }) {
  const field = useField();
  const [open, setOpen] = useState(false), [active, setActive] = useState(0);
  const [filtering, setFiltering] = useState(false);
  const listId = useId(), input = useRef<HTMLInputElement>(null);
  const query = value.trim().toLowerCase();
  const options = suggestions.map(item => typeof item === 'string' ? { value: item, label: item } : item);
  const items = (filtering && query ? options.filter(item => item.value.toLowerCase().includes(query) || item.label.toLowerCase().includes(query)) : options).slice(0, 50);
  const hasSuggestions = options.length > 0 || !!emptyText;
  const expanded = open && !readOnly && (items.length > 0 || !!emptyText);
  const selected = items.findIndex(item => item.value === value);
  useEffect(() => { setActive(Math.max(selected, 0)); }, [value, open, selected]);
  const choose = (item: { value: string; label: string }) => { onChange(item.value); setOpen(false); };
  return <Popover.Root open={expanded} onOpenChange={setOpen}>
    <Popover.Anchor asChild>
      <div className="k-combobox"><input ref={input} id={field?.id} name={name} className="k-input" value={value} placeholder={placeholder} readOnly={readOnly} required={required} maxLength={maxLength} autoComplete="off"
        role={hasSuggestions ? 'combobox' : undefined} aria-expanded={hasSuggestions ? expanded : undefined} aria-controls={expanded ? listId : undefined} aria-autocomplete={hasSuggestions ? 'list' : undefined} aria-describedby={field?.describedBy} aria-invalid={field?.invalid || undefined}
        aria-activedescendant={expanded && items[active] ? `${listId}-${active}` : undefined}
        onFocus={() => { setFiltering(false); setOpen(true); }} onClick={() => setOpen(true)} onChange={event => { onChange(event.target.value); setFiltering(true); setOpen(true); }}
        onKeyDown={event => {
          if (readOnly) return;
          if (event.key === 'ArrowDown' && items.length) { event.preventDefault(); setOpen(true); setActive(index => Math.min(index + 1, items.length - 1)); }
          else if (event.key === 'ArrowUp' && expanded) { event.preventDefault(); setActive(index => Math.max(index - 1, 0)); }
          else if (event.key === 'Enter' && expanded && items[active]) { event.preventDefault(); choose(items[active]); }
          else if (event.key === 'Escape' && expanded) { event.preventDefault(); event.stopPropagation(); setOpen(false); }
        }} />{!readOnly && hasSuggestions && <IconButton label={`Show ${name === 'backupModel' ? 'fallback' : name ?? 'options'} suggestions`} className="k-combobox__toggle" onClick={() => { setFiltering(false); setOpen(true); input.current?.focus(); }}><ChevronDown size={14} aria-hidden /></IconButton>}</div>
    </Popover.Anchor>
    <Popover.Portal>
      <Popover.Content className="k-pop k-combobox__content" align="start" sideOffset={6} onOpenAutoFocus={event => event.preventDefault()} onInteractOutside={event => { if ((event.target as HTMLElement | null)?.id === field?.id) event.preventDefault(); }}>
        <ul id={listId} role="listbox" className="k-combobox__list">
          {items.map((item, index) => <li key={item.value} id={`${listId}-${index}`} role="option" aria-selected={item.value === value} className={cx('k-menu__item', index === active && 'is-active')} onMouseDown={event => event.preventDefault()} onMouseEnter={() => setActive(index)} onClick={() => choose(item)}>
            <span className={cx('k-combobox__value', item.label !== item.value && 'is-label')}>{item.label}</span>{item.value === value && <Check size={14} aria-hidden />}
          </li>)}
          {!items.length && emptyText && <li className="k-combobox__empty">{emptyText}</li>}
        </ul>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}

export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (checked: boolean) => void; label: string; disabled?: boolean }) {
  return <RadixSwitch.Root className="k-switch" checked={checked} onCheckedChange={onChange} disabled={disabled} aria-label={label}><RadixSwitch.Thumb className="k-switch__thumb" /></RadixSwitch.Root>;
}

export function Checkbox({ checked, onChange, label, description, disabled }: { checked: boolean; onChange: (checked: boolean) => void; label: ReactNode; description?: ReactNode; disabled?: boolean }) {
  const id = useId();
  return <div className={cx('k-check', disabled && 'is-disabled')}>
    <RadixCheckbox.Root id={id} className="k-check__box" checked={checked} onCheckedChange={value => onChange(value === true)} disabled={disabled}><RadixCheckbox.Indicator><Check size={12} strokeWidth={3} aria-hidden /></RadixCheckbox.Indicator></RadixCheckbox.Root>
    <label htmlFor={id} className="k-check__text"><span className="k-check__label">{label}</span>{description && <span className="k-check__desc">{description}</span>}</label>
  </div>;
}

/** Tabs for switching views inside one page or dialog. */
export function Tabs<T extends string>({ value, onChange, tabs, label, children }: { value: T; onChange: (value: T) => void; tabs: { value: T; label: ReactNode; count?: number; title?: string }[]; label: string; children?: ReactNode }) {
  return <RadixTabs.Root value={value} onValueChange={next => onChange(next as T)} className="k-tabs">
    <RadixTabs.List className="k-tabs__list" aria-label={label}>{tabs.map(tab => <RadixTabs.Trigger key={tab.value} value={tab.value} title={tab.title} className="k-tabs__tab">{tab.label}{tab.count !== undefined && <span className="k-tabs__count">{tab.count}</span>}</RadixTabs.Trigger>)}</RadixTabs.List>
    {children}
  </RadixTabs.Root>;
}
export function TabPanel({ value, children }: { value: string; children: ReactNode }) { return <RadixTabs.Content value={value} tabIndex={-1} className="k-tabs__panel">{children}</RadixTabs.Content>; }

/** Scrollable read-only text (file contents, JSON, logs). */
export function CodeBlock({ children, label, maxHeight = 280 }: { children: ReactNode; label: string; maxHeight?: number }) {
  return <pre className="k-code" tabIndex={0} role="region" aria-label={label} style={{ maxHeight }}>{children}</pre>;
}

export function Segmented<T extends string>({ value, onChange, options, label, disabled }: { value: T; onChange: (value: T) => void; options: { value: T; label: ReactNode; description?: ReactNode }[]; label: string; disabled?: boolean }) {
  return <div className="k-choice" role="radiogroup" aria-label={label} onKeyDown={event => {
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
    const index = buttons.indexOf(event.target as HTMLButtonElement);
    if (index < 0) return;
    let next: number;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % buttons.length;
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + buttons.length) % buttons.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = buttons.length - 1;
    else return;
    event.preventDefault();
    buttons[next]?.focus();
    buttons[next]?.click();
  }}>
    {options.map(option => <button key={option.value} type="button" role="radio" aria-checked={value === option.value} tabIndex={value === option.value ? 0 : -1} disabled={disabled} className={cx('k-choice__opt', value === option.value && 'is-active')} onClick={() => onChange(option.value)}>
      <span className="k-choice__title">{option.label}</span>{option.description && <span className="k-choice__sub">{option.description}</span>}
    </button>)}
  </div>;
}

// ---------- surfaces ----------
export function Alert({ tone = 'info', title, children, actions }: { tone?: 'info' | 'warn' | 'error'; title?: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  const Icon = tone === 'error' ? CircleAlert : tone === 'warn' ? AlertTriangle : Info;
  return <div className={cx('k-alert', `is-${tone}`)} role={tone === 'error' ? 'alert' : 'status'}>
    <Icon size={16} aria-hidden className="k-alert__icon" />
    <div className="k-alert__body">{title && <div className="k-alert__title">{title}</div>}{children && <div className="k-alert__text">{children}</div>}{actions && <div className="k-alert__actions">{actions}</div>}</div>
  </div>;
}
export function Progress({ children }: { children: ReactNode }) {
  return <span className="k-progress" role="status" aria-live="polite"><span className="k-spinner" aria-hidden />{children}</span>;
}
/** Saved commands also exist while requests run; recovery actions only make sense after settlement. */
export function RecoveryAlert({ inFlight, progress = 'Applying change...', ...props }: { inFlight: boolean; progress?: ReactNode; tone?: 'warn' | 'error'; title?: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  const { tone = 'warn', ...alert } = props;
  return inFlight ? (progress ? <Progress>{progress}</Progress> : null) : <Alert tone={tone} {...alert} />;
}
export function Section({ title, actions, children, id }: { title: ReactNode; actions?: ReactNode; children: ReactNode; id?: string }) {
  return <section className="k-section" aria-labelledby={id}>
    <div className="k-section__head"><h2 className="k-section__title" id={id}>{title}</h2>{actions && <div className="k-section__actions">{actions}</div>}</div>
    {children}
  </section>;
}
export function Card({ children, className }: { children: ReactNode; className?: string }) { return <div className={cx('k-card', className)}>{children}</div>; }
export function Row({ title, meta, leading, trailing, onClick, disabled, children }: { title: ReactNode; meta?: ReactNode; leading?: ReactNode; trailing?: ReactNode; onClick?: () => void; disabled?: boolean; children?: ReactNode }) {
  const body = <>{leading && <span className="k-row__leading">{leading}</span>}<span className="k-row__text"><span className="k-row__title">{title}</span>{meta && <span className="k-row__meta">{meta}</span>}{children}</span></>;
  return <div className={cx('k-row', onClick && 'is-clickable')}>
    {onClick ? <button type="button" className="k-row__main" onClick={onClick} disabled={disabled}>{body}{!trailing && <ChevronRight size={16} aria-hidden className="k-row__chev" />}</button> : <div className="k-row__main">{body}</div>}
    {trailing && <span className="k-row__trailing">{trailing}</span>}
  </div>;
}
export function Badge({ children, tone = 'neutral' }: { children: ReactNode; tone?: 'neutral' | 'brand' | 'success' | 'warn' }) { return <span className={cx('k-badge', `is-${tone}`)}>{children}</span>; }
export function EmptyState({ icon, title, children, action }: { icon?: ReactNode; title: ReactNode; children?: ReactNode; action?: ReactNode }) {
  return <div className="k-empty">{icon && <span className="k-empty__icon" aria-hidden>{icon}</span>}<div className="k-empty__title">{title}</div>{children && <div className="k-empty__text">{children}</div>}{action}</div>;
}
export function Delayed({ children }: { children: ReactNode }) {
  return useDelayedVisible() ? children : null;
}
export function SkeletonRows({ count = 3 }: { count?: number }) {
  return <Delayed><div className="k-card" aria-hidden>{Array.from({ length: count }, (_, index) => <div key={index} className="k-row"><div className="k-row__main"><span className="k-row__text"><span className="k-skeleton" style={{ width: `${46 + (index * 17) % 30}%` }} /><span className="k-skeleton is-small" style={{ width: `${22 + (index * 11) % 20}%` }} /></span></div></div>)}</div></Delayed>;
}
export function PageHeader({ title, description, actions, headingRef }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; headingRef?: Ref<HTMLHeadingElement> }) {
  return <header className="k-page-head"><div className="k-page-head__text"><h1 ref={headingRef} tabIndex={-1}>{title}</h1>{description && <p>{description}</p>}</div>{actions && <div className="k-page-head__actions">{actions}</div>}</header>;
}

/** Disclosure. Pass `open` and `onOpenChange` for controlled state. */
export function Collapsible({ title, children, defaultOpen = false, open, onOpenChange, variant = 'inline' }: { title: ReactNode; children: ReactNode; defaultOpen?: boolean; open?: boolean; onOpenChange?: (open: boolean) => void; variant?: 'inline' | 'section' }) {
  return <RadixCollapsible.Root className="k-collapsible" defaultOpen={defaultOpen} open={open} onOpenChange={onOpenChange}>
    <RadixCollapsible.Trigger className={cx('k-collapsible__trigger', variant === 'section' && 'k-collapsible__trigger--section')}>
      <span className="k-collapsible__title">{title}</span><ChevronDown size={15} aria-hidden className="k-collapsible__chev" />
    </RadixCollapsible.Trigger>
    {/* Force-mounted so fields inside stay part of their form while collapsed. */}
    <RadixCollapsible.Content forceMount className="k-collapsible__content">{children}</RadixCollapsible.Content>
  </RadixCollapsible.Root>;
}

export function Tooltip({ content, children, side, sideOffset = 6 }: { content: ReactNode; children: ReactNode; side?: 'top' | 'right' | 'bottom' | 'left'; sideOffset?: number }) {
  return <RadixTooltip.Provider delayDuration={400}><RadixTooltip.Root><RadixTooltip.Trigger asChild onFocus={event => { if (!(event.target as HTMLElement).matches(':focus-visible')) event.preventDefault(); }}>{children}</RadixTooltip.Trigger><RadixTooltip.Portal><RadixTooltip.Content className="k-tooltip" side={side} sideOffset={sideOffset}>{content}</RadixTooltip.Content></RadixTooltip.Portal></RadixTooltip.Root></RadixTooltip.Provider>;
}

export type MenuEntry = { label: string; onSelect: () => void; prefetch?: () => void; danger?: boolean; disabled?: boolean; icon?: ReactNode; hint?: string } | 'separator';
export function ActionMenu({ trigger, items, align = 'end', label, onCloseAutoFocus }: { onCloseAutoFocus?: (event: Event) => void; trigger: ReactNode; items: MenuEntry[]; align?: 'start' | 'center' | 'end'; label: string }) {
  return <DropdownMenu.Root>
    <DropdownMenu.Trigger asChild aria-label={label}>{trigger}</DropdownMenu.Trigger>
    <DropdownMenu.Portal><DropdownMenu.Content className="k-pop k-menu" align={align} sideOffset={6} onCloseAutoFocus={onCloseAutoFocus}>
      {items.map((item, index) => item === 'separator' ? <DropdownMenu.Separator key={index} className="k-menu__sep" /> : <DropdownMenu.Item key={item.label} disabled={item.disabled} onSelect={item.onSelect} onPointerEnter={item.prefetch} onFocus={item.prefetch} className={cx('k-menu__item', item.danger && 'is-danger')}>{item.icon}{item.label}{item.hint && <span className="k-menu__hint">{item.hint}</span>}</DropdownMenu.Item>)}
    </DropdownMenu.Content></DropdownMenu.Portal>
  </DropdownMenu.Root>;
}
export function ActionPopover({ trigger, children, label }: { trigger: ReactNode; children: ReactNode; label: string }) {
  return <Popover.Root><Popover.Trigger asChild>{trigger}</Popover.Trigger><Popover.Portal><Popover.Content className="k-pop k-action-popover bs-root" aria-label={label} align="end" sideOffset={8}>{children}</Popover.Content></Popover.Portal></Popover.Root>;
}

// ---------- dialogs ----------
export function Dialog({ open, onOpenChange, title, description, actions, children, footer, size = 'md', hideHeader = false, onCloseAutoFocus }: { onCloseAutoFocus?: (event: Event) => void; open: boolean; onOpenChange: (open: boolean) => void; title: ReactNode; description?: ReactNode; actions?: ReactNode; children: ReactNode; footer?: ReactNode; size?: 'md' | 'lg'; hideHeader?: boolean }) {
  // Code-opened dialogs remember the opener and restore focus on close.
  const opener = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => { if (open) opener.current = document.activeElement instanceof HTMLElement && !document.activeElement.closest('.k-dialog') ? document.activeElement : opener.current ?? null; }, [open]);
  return <RadixDialog.Root open={open} onOpenChange={onOpenChange}>
    <RadixDialog.Portal>
      <RadixDialog.Overlay className="k-overlay" />
      <RadixDialog.Content {...(!description ? { 'aria-describedby': undefined } : {})} className={cx('k-dialog', 'bs-root', `k-dialog--${size}`)} onCloseAutoFocus={event => { onCloseAutoFocus?.(event); if (event.defaultPrevented) return; const target = opener.current; opener.current = null; if (target?.isConnected) { event.preventDefault(); target.focus(); } }} onOpenAutoFocus={event => {
        const body = (event.currentTarget as HTMLElement).querySelector('.k-dialog__body');
        const first = body?.querySelector<HTMLElement>(':is(input:not([type=hidden]):not([type=number]), textarea):not([readonly]):not(:disabled)') ?? body?.querySelector<HTMLElement>('[role=tab][data-state=active]');
        event.preventDefault(); (first ?? (event.currentTarget as HTMLElement)).focus();
      }}>
        {hideHeader ? <><RadixDialog.Title className="sr-only">{title}</RadixDialog.Title>{description && <RadixDialog.Description className="sr-only">{description}</RadixDialog.Description>}</> : <div className="k-dialog__head">
          <div><RadixDialog.Title className="k-dialog__title">{title}</RadixDialog.Title>{description ? <RadixDialog.Description className="k-dialog__desc">{description}</RadixDialog.Description> : null}</div>
          {actions}<RadixDialog.Close asChild><IconButton label="Close"><X size={16} aria-hidden /></IconButton></RadixDialog.Close>
        </div>}
        <div className="k-dialog__body">{children}</div>
        {footer && <div className="k-dialog__foot">{footer}</div>}
      </RadixDialog.Content>
    </RadixDialog.Portal>
  </RadixDialog.Root>;
}

type ConfirmRequest = { title: string; description?: ReactNode; confirm?: string; danger?: boolean; resolve: (ok: boolean) => void };
const ConfirmCtx = createContext<((request: Omit<ConfirmRequest, 'resolve'>) => Promise<boolean>) | null>(null);
/** Promise-based confirmation that replaces window.confirm with an accessible alert dialog. */
export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<ConfirmRequest | null>(null);
  // Confirms open from code, not from a Radix trigger, so remember where focus was and return it on close.
  const returnFocus = useRef<HTMLElement | null>(null);
  const ask = useCallback((next: Omit<ConfirmRequest, 'resolve'>) => new Promise<boolean>(resolve => { returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; setRequest({ ...next, resolve }); }), []);
  const settle = (ok: boolean) => { request?.resolve(ok); setRequest(null); };
  return <ConfirmCtx.Provider value={ask}>
    {children}
    <AlertDialog.Root open={!!request} onOpenChange={open => { if (!open) settle(false); }}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="k-overlay" />
        <AlertDialog.Content className="k-dialog k-dialog--sm" onCloseAutoFocus={event => { const target = returnFocus.current; returnFocus.current = null; if (target?.isConnected) { event.preventDefault(); target.focus(); } }}>
          <div className="k-dialog__head"><div><AlertDialog.Title className="k-dialog__title">{request?.title}</AlertDialog.Title>{request?.description ? <AlertDialog.Description className="k-dialog__desc">{request.description}</AlertDialog.Description> : <AlertDialog.Description className="sr-only">{request?.title}</AlertDialog.Description>}</div></div>
          <div className="k-dialog__foot">
            <AlertDialog.Cancel asChild><Button>Cancel</Button></AlertDialog.Cancel>
            <AlertDialog.Action asChild><Button variant={request?.danger ? 'danger' : 'primary'} onClick={() => settle(true)}>{request?.confirm ?? 'Continue'}</Button></AlertDialog.Action>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  </ConfirmCtx.Provider>;
}
export function useConfirm() {
  const ask = useContext(ConfirmCtx);
  const fallback = useRef((request: Omit<ConfirmRequest, 'resolve'>) => Promise.resolve(window.confirm(request.title)));
  return ask ?? fallback.current;
}
