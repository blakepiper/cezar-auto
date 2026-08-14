import { z } from 'zod';

/** User-facing reasoning policy. `auto` is resolved independently for each agent chunk. */
export const reasoningEffortSchema = z.enum(['auto', 'low', 'medium', 'high', 'xhigh']);
export type ReasoningEffort = z.infer<typeof reasoningEffortSchema>;

/** A concrete level sent to a backend after the run manager resolves `auto`. */
export const concreteReasoningEffortSchema = reasoningEffortSchema.exclude(['auto']);
export type ConcreteReasoningEffort = z.infer<typeof concreteReasoningEffortSchema>;
