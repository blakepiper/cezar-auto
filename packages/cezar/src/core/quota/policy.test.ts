import { describe, expect, it } from 'vitest';
import { loadWorkspaceConfig } from '../../workspace/config.ts';
import { quotaRoutingPolicy } from './policy.ts';

describe('quotaRoutingPolicy', () => {
  it('maps only the coordinator policy from safe workspace defaults', async () => {
    const policy = quotaRoutingPolicy(await loadWorkspaceConfig());

    expect(policy).toEqual({
      enabled: false,
      providerOrder: ['claude', 'codex'],
      unknownUsagePolicy: 'allow',
      providers: {
        claude: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 95, resumeBelowPercent: 80, maxConcurrent: 1 },
        codex: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 90, resumeBelowPercent: 80, maxConcurrent: 1 },
      },
    });
  });
});
