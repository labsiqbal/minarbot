import { CalendarClock, CircleDot, Hash, ListTodo, Siren } from 'lucide-react';
import { Card, Dialog, Row } from './ui/kit';

export const routineTemplates = {
  slack: { name: 'Slack message', icon: Hash, prompt: 'Summarize the Slack message and flag any follow-up.' },
  linear: { name: 'Linear issue', icon: ListTodo, prompt: 'Summarize the Linear issue and identify its next step.' },
  sentry: { name: 'Sentry alert', icon: Siren, prompt: 'Summarize the Sentry alert and suggest a first diagnostic step.' },
  github: { name: 'GitHub issue', icon: CircleDot, prompt: 'Summarize the GitHub issue and identify its next step.' },
  schedule: { name: 'Schedule', icon: CalendarClock, prompt: '' },
};
export type RoutineTemplate = keyof typeof routineTemplates;
export type WebhookSource = Exclude<RoutineTemplate, 'schedule'>;

export function RoutineTemplatePicker({ onPick, onClose }: { onPick: (template: RoutineTemplate) => void; onClose: () => void }) {
  return <Dialog open title="New from template" onOpenChange={open => { if (!open) onClose(); }}>
    <Card>{(Object.keys(routineTemplates) as RoutineTemplate[]).map(template => {
      const { name, icon: Icon } = routineTemplates[template];
      return <Row key={template} title={name} leading={<Icon size={16} aria-hidden />} onClick={() => onPick(template)} />;
    })}</Card>
  </Dialog>;
}
