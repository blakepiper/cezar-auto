import { z } from 'zod';

/** Relative path used to select an IDE directory. The empty value means the project root. */
export const ideDirectoryQuerySchema = z.object({
  path: z.string().max(4_096).optional(),
}).strict();

/** Relative path used to select one editable project file. */
export const ideFileQuerySchema = z.object({
  path: z.string().min(1).max(4_096),
}).strict();

export const ideFileInputSchema = z.object({
  path: z.string().min(1).max(4_096),
  content: z.string().max(1_000_000),
}).strict();
export type IdeFileInput = z.infer<typeof ideFileInputSchema>;

export const ideEntrySchema = z.object({
  name: z.string(),
  path: z.string(),
  type: z.enum(['dir', 'file']),
  size: z.number().int().nonnegative().optional(),
});
export type IdeEntry = z.infer<typeof ideEntrySchema>;

export const ideDirectoryResponseSchema = z.object({
  path: z.string(),
  entries: z.array(ideEntrySchema),
  truncated: z.boolean(),
});
export type IdeDirectoryResponse = z.infer<typeof ideDirectoryResponseSchema>;

export const ideFileResponseSchema = z.object({
  path: z.string(),
  content: z.string(),
  size: z.number().int().nonnegative(),
});
export type IdeFileResponse = z.infer<typeof ideFileResponseSchema>;
