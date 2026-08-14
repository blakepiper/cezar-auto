import type { ConcreteReasoningEffort, ReasoningEffort } from '@open-mercato/cezar-contract';

const HIGH_SIGNAL = /\b(architect(?:ure|ural)?|debug(?:ging)?|root cause|investigat(?:e|ion)|migrat(?:e|ion)|security|auth(?:entication|orization)?|concurren(?:cy|t)|performance|refactor(?:ing)?|race condition|data loss|production outage|breaking change)\b/gi;
const LOW_SIGNAL = /\b(format|rename|copy|typo|wording|docs?|readme|comment|label|style|css|color|lint|typecheck|test(?:s|ing)?|verify|check|bump version)\b/gi;

/** Resolve the authored policy at the chunk boundary. Auto stays deterministic and explainable,
 * while a fresh session gets a fresh decision from its current task/step prompt. */
export function resolveReasoningEffort(
  requested: ReasoningEffort | undefined,
  context: { task: string; prompt: string; stepName?: string },
): ConcreteReasoningEffort {
  if (requested && requested !== 'auto') return requested;

  const text = `${context.stepName ?? ''}\n${context.task}\n${context.prompt}`.trim();
  const highSignals = text.match(HIGH_SIGNAL)?.length ?? 0;
  const lowSignals = text.match(LOW_SIGNAL)?.length ?? 0;
  const long = text.length >= 4_000;

  if (highSignals >= 2 || text.length >= 10_000) return 'xhigh';
  if (highSignals > 0 || long) return 'high';
  if (lowSignals > 0 && highSignals === 0 && text.length < 1_800) return 'low';
  return 'medium';
}
