import { RadioGroup, Toast } from 'radix-ui';
import { createRoot } from 'react-dom/client';
import { Check } from 'lucide-react';
import { Button } from './kit';
import './avatar.css';

import looks from './avatar-looks.json';

// Original Minarbot silhouettes and fixed colors shared with the Rust picker.
export const AVATAR_SHAPES = looks.shapes;
export const AVATAR_COLORS = looks.colors;
export type AvatarLook = { shape: number; color: number };
const eyeFill = '#1C1C1C';
const eyes = 'M43 49a3.5 3.5 0 0 0-7 0v5a3.5 3.5 0 0 0 7 0ZM64 49a3.5 3.5 0 0 0-7 0v5a3.5 3.5 0 0 0 7 0Z';
function parts(look: AvatarLook) {
  const shape = AVATAR_SHAPES[look.shape], color = AVATAR_COLORS[look.color];
  if (!shape || !color) throw new Error('Choose an available avatar shape and color.');
  return { shape, color };
}
export function MascotAvatar({ look, size = 64 }: { look: AvatarLook; size?: number }) {
  const { shape, color } = parts(look);
  return <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width={size} height={size} aria-hidden="true"><path d={shape.path} fill={color.fill} /><path d={eyes} fill={eyeFill} /></svg>;
}
export async function avatarPng(look: AvatarLook): Promise<string> {
  const { shape, color } = parts(look);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="256" height="256"><path d="${shape.path}" fill="${color.fill}"/><path d="${eyes}" fill="${eyeFill}"/></svg>`;
  const image = new Image(); image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`; await image.decode();
  const canvas = document.createElement('canvas'); canvas.width = canvas.height = 256;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('Avatar could not render. Try uploading an image instead.');
  context.drawImage(image, 0, 0, 256, 256);
  return canvas.toDataURL('image/png').split(',')[1];
}
export function AvatarPicker({ name = '', value, onChange, disabled = false }: { name?: string; value: AvatarLook | null; onChange: (look: AvatarLook | null) => void; disabled?: boolean }) {
  const color = value?.color ?? 0;
  return <div className="k-avatar-picker bs-root">
    <div className="k-avatar-picker__head">
      <div className="k-avatar-picker__preview" role="img" aria-label={value ? `${AVATAR_COLORS[color].name} ${AVATAR_SHAPES[value.shape].name} preview` : 'Initials avatar preview'}>{value ? <MascotAvatar look={value} size={76} /> : <span className="bs-avatar" aria-hidden="true">{name.trim().slice(0, 1).toUpperCase() || '?'}</span>}</div>
      <div className="k-avatar-picker__intro"><span className="k-field__label">Choose a look <span className="k-field__optional">Optional</span></span><span className="k-field__hint">Pick a shape and color, or keep initials.</span>{value && <Button size="sm" variant="ghost" disabled={disabled} onClick={() => onChange(null)}>Clear choice</Button>}</div>
    </div>
    <RadioGroup.Root className="k-avatar-picker__shapes" aria-label="Avatar shape" value={value ? String(value.shape) : ''} disabled={disabled} orientation="horizontal" onValueChange={shape => onChange({ shape: Number(shape), color })} loop>
      {AVATAR_SHAPES.map((shape, index) => <RadioGroup.Item key={shape.name} className="k-avatar-picker__shape" value={String(index)} aria-label={shape.name} title={shape.name}><MascotAvatar look={{ shape: index, color }} size={42} /><RadioGroup.Indicator className="k-avatar-picker__check"><Check size={10} aria-hidden /></RadioGroup.Indicator></RadioGroup.Item>)}
    </RadioGroup.Root>
    <RadioGroup.Root className="k-avatar-picker__colors" aria-label="Avatar color" value={String(color)} disabled={disabled} onValueChange={next => onChange({ shape: value?.shape ?? 0, color: Number(next) })} orientation="horizontal" loop>
      {AVATAR_COLORS.map((swatch, index) => <RadioGroup.Item key={swatch.name} className="k-avatar-picker__swatch" value={String(index)} aria-label={swatch.name} title={swatch.name}><span style={{ background: swatch.fill }}><RadioGroup.Indicator><Check size={14} color={eyeFill} aria-hidden /></RadioGroup.Indicator></span></RadioGroup.Item>)}
    </RadioGroup.Root>
  </div>;
}

export function notifyAvatarFailure() {
  const host = document.createElement('div'); document.body.append(host);
  const root = createRoot(host);
  root.render(<Toast.Provider><Toast.Root defaultOpen duration={12000} type="foreground" className="k-avatar-toast bs-root" onOpenChange={open => { if (!open) queueMicrotask(() => { root.unmount(); host.remove(); }); }}>
    <Toast.Title className="k-field__label">Agent created</Toast.Title>
    <Toast.Description>Avatar could not save. Open Edit profile to try again.</Toast.Description>
    <Toast.Close asChild><Button size="sm">Dismiss</Button></Toast.Close>
  </Toast.Root><Toast.Viewport className="k-avatar-toast-viewport" /></Toast.Provider>);
}
