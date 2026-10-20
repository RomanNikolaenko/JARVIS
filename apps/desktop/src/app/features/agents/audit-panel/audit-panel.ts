import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import type { AuditEvent, RiskLevel } from '@jarvis/shared-types';

import { AgentsStore } from '../../../core/state/agents.store';
import { formatClock, formatShortDate } from '../../../shared/format';
import { Icon } from '../../../shared/icon/icon';

const EVENTS: Record<AuditEvent, { label: string; tone: string }> = {
  allowed: { label: 'allowed', tone: 'accent' },
  approved: { label: 'approved by you', tone: 'accent' },
  denied: { label: 'denied by you', tone: 'warning' },
  policyDenied: { label: 'refused by policy', tone: 'danger' },
  timedOut: { label: 'not approved in time', tone: 'warning' },
  cancelled: { label: 'cancelled', tone: '' },
  succeeded: { label: 'succeeded', tone: 'success' },
  failed: { label: 'failed', tone: 'danger' },
};

const RISK_TONE: Record<RiskLevel, string> = { low: 'success', medium: 'warning', high: 'danger' };

/** The hash-chained audit log of tool calls. */
@Component({
  selector: 'jv-audit-panel',
  imports: [Icon],
  templateUrl: './audit-panel.html',
  styleUrls: ['../agents-shared.scss', './audit-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class AuditPanel {
  protected readonly store = inject(AgentsStore);

  constructor() {
    void this.store.loadAudit();
  }

  /** Time for today's entries, date and time for older ones. */
  protected when(timestampMs: number): string {
    const today = new Date(timestampMs).toDateString() === new Date().toDateString();
    return today ? formatClock(timestampMs) : `${formatShortDate(timestampMs)} ${formatClock(timestampMs)}`;
  }

  protected eventLabel(event: AuditEvent): string {
    return EVENTS[event].label;
  }

  protected eventTone(event: AuditEvent): string {
    return EVENTS[event].tone;
  }

  protected riskTone(risk: RiskLevel): string {
    return RISK_TONE[risk];
  }
}
