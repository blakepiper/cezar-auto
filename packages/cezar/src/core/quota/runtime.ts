import { FileProviderUsageSnapshotStore } from '../../workspace/provider-usage.ts';
import { loadWorkspaceConfig, type WorkspaceConfig } from '../../workspace/config.ts';
import { createInstalledClaudeUsageAdapter } from './claude-usage-adapter.ts';
import { createInstalledCodexUsageAdapter } from './codex-usage-adapter.ts';
import { QuotaCoordinator } from './coordinator.ts';
import { quotaRoutingPolicy } from './policy.ts';
import { ProviderUsageService } from './usage-service.ts';

export interface QuotaRuntime {
  usage: ProviderUsageService;
  coordinator: QuotaCoordinator;
  updateConfig(config: WorkspaceConfig): void;
  dispose(): void;
}

/** One process-wide quota runtime; project managers share its reservations and cache. */
export async function createQuotaRuntime(
  repoRoot: string,
  config?: WorkspaceConfig,
): Promise<QuotaRuntime> {
  const workspaceConfig = config ?? await loadWorkspaceConfig();
  const usage = new ProviderUsageService({
    adapters: [
      createInstalledClaudeUsageAdapter({ timeoutMs: workspaceConfig.quotaRouting.requestTimeoutSeconds * 1_000 }),
      createInstalledCodexUsageAdapter({
        cwd: repoRoot,
        timeoutMs: workspaceConfig.quotaRouting.requestTimeoutSeconds * 1_000,
      }),
    ],
    cacheTtlMs: workspaceConfig.quotaRouting.cacheTtlSeconds * 1_000,
    refreshIntervalMs: workspaceConfig.quotaRouting.refreshIntervalSeconds * 1_000,
    store: new FileProviderUsageSnapshotStore(),
  });
  await usage.restore();
  const coordinator = new QuotaCoordinator(usage, () => quotaRoutingPolicy(workspaceConfig));
  return {
    usage,
    coordinator,
    updateConfig: (next) => coordinator.setPolicy(quotaRoutingPolicy(next)),
    dispose: () => {
      coordinator.dispose();
      usage.dispose();
    },
  };
}
