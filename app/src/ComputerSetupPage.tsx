import { ComputerSetup } from './ComputerSetup';
import type { ComputerRequest } from './AgentComputerPanel';
import { PageHeader } from './ui/kit';

const focusHeading = (heading: HTMLHeadingElement | null) => heading?.focus();

export function ComputerSetupPage({ request }: { request: ComputerRequest }) {
  return <div className="k-page bs-root">
    <PageHeader headingRef={focusHeading} title="Computer" />
    <ComputerSetup request={request} />
  </div>;
}
