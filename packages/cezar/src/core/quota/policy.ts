import type { WorkspaceConfig } from '../../workspace/config.ts';
import type { QuotaRoutingPolicy } from './router.ts';

/**
 * The coordinator deliberately accepts only its synchronous, execution-relevant
 * policy. Keep workspace persistence details (refresh cadence and request
 * timeout) out of the pure router.
 */
export function quotaRoutingPolicy(config: WorkspaceConfig): QuotaRoutingPolicy {
  const { quotaRouting } = config;
  return {
    enabled: quotaRouting.enabled,
    providerOrder: quotaRouting.providerOrder,
    unknownUsagePolicy: quotaRouting.unknownUsagePolicy,
    providers: {
      claude: {
        enabled: quotaRouting.providers.claude.enabled,
        stopNewWorkAtPercent: quotaRouting.providers.claude.stopNewWorkAtPercent,
        longWindowStopAtPercent: quotaRouting.providers.claude.longWindowStopAtPercent,
        resumeBelowPercent: quotaRouting.providers.claude.resumeBelowPercent,
        maxConcurrent: quotaRouting.providers.claude.maxConcurrent,
      },
      codex: {
        enabled: quotaRouting.providers.codex.enabled,
        stopNewWorkAtPercent: quotaRouting.providers.codex.stopNewWorkAtPercent,
        longWindowStopAtPercent: quotaRouting.providers.codex.longWindowStopAtPercent,
        resumeBelowPercent: quotaRouting.providers.codex.resumeBelowPercent,
        maxConcurrent: quotaRouting.providers.codex.maxConcurrent,
      },
    },
  };
}
